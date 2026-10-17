#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/input-event-codes.h>
#include <limits.h>
#include <poll.h>
#include <stdint.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>
#include <wayland-client.h>
#include <wayland-client-protocol.h>
#include <xkbcommon/xkbcommon.h>
#include "virtual-keyboard-unstable-v1-client-protocol.h"

#define CONTROL_PATH "/run/pigade-ime/ime-keyboard.sock"
#define MAX_COMMAND 16
#define CONTROL_TIMEOUT_MS 20000
#define PEER_UID getuid()

enum fixed_command { COMMAND_INVALID, COMMAND_COMPOSE, COMMAND_COMMIT, COMMAND_QUIT };
enum input_state { INPUT_READY, INPUT_COMPOSED, INPUT_COMMITTED, INPUT_QUIT };

struct globals {
    struct wl_seat *seat;
    struct zwp_virtual_keyboard_manager_v1 *manager;
    unsigned seats;
    unsigned managers;
};

static void fail(const char *message) {
    fprintf(stderr, "ime-input: %s\n", message);
    exit(2);
}

static void registry_global(void *data, struct wl_registry *registry,
                            uint32_t name, const char *interface, uint32_t version) {
    struct globals *g = data;
    if (strcmp(interface, wl_seat_interface.name) == 0) {
        g->seats++;
        if (g->seats == 1) {
            g->seat = wl_registry_bind(registry, name, &wl_seat_interface,
                                       version < 4 ? version : 4);
        }
    } else if (strcmp(interface, zwp_virtual_keyboard_manager_v1_interface.name) == 0) {
        g->managers++;
        if (g->managers == 1) {
            g->manager = wl_registry_bind(registry, name,
                &zwp_virtual_keyboard_manager_v1_interface, 1);
        }
    }
}

static void registry_remove(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data; (void)registry; (void)name;
}

static const struct wl_registry_listener registry_listener = {
    .global = registry_global,
    .global_remove = registry_remove,
};

static uint32_t last_key_time;

static void check_peer(struct wl_display *display, pid_t expected_pid) {
    struct ucred cred;
    socklen_t size = sizeof(cred);
    int fd = wl_display_get_fd(display);
    if (getsockopt(fd, SOL_SOCKET, SO_PEERCRED, &cred, &size) != 0 ||
        size != sizeof(cred) || cred.pid != expected_pid ||
        cred.uid != PEER_UID || cred.gid != getgid()) {
        fail("Wayland socket peer does not match the exact private Sway process");
    }
}

static int make_control_socket(void) {
    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0) fail("cannot create control socket");
    struct sockaddr_un address = {.sun_family = AF_UNIX};
    if (strlen(CONTROL_PATH) >= sizeof(address.sun_path)) fail("control socket path too long");
    strcpy(address.sun_path, CONTROL_PATH);
    if (connect(fd, (struct sockaddr *)&address, sizeof(address)) != 0)
        fail("cannot connect to parent control socket");
    struct ucred cred;
    socklen_t size = sizeof(cred);
    if (getsockopt(fd, SOL_SOCKET, SO_PEERCRED, &cred, &size) != 0 ||
        size != sizeof(cred) || cred.pid != getppid() || cred.uid != PEER_UID ||
        cred.gid != getgid()) fail("control socket peer is not the owning runner");
    return fd;
}

static int keymap_fd(const char *keymap, size_t size) {
    int fd = memfd_create("pigade-ime-us-keymap", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    if (fd < 0) fail("cannot create fixed keymap memfd");
    size_t offset = 0;
    while (offset < size) {
        ssize_t n = write(fd, keymap + offset, size - offset);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) fail("cannot write fixed keymap");
        offset += (size_t)n;
    }
    if (lseek(fd, 0, SEEK_SET) < 0) fail("cannot rewind fixed keymap");
    return fd;
}

static uint64_t monotonic_ms(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) fail("monotonic clock unavailable");
    return (uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u;
}

static uint32_t wayland_key_time_ms(void) {
    return (uint32_t)monotonic_ms();
}

static void send_key(struct zwp_virtual_keyboard_v1 *keyboard, uint32_t key) {
    uint32_t time = wayland_key_time_ms();
    if ((int32_t)(time - last_key_time) <= 0) time = last_key_time + 1;
    last_key_time = time;
    zwp_virtual_keyboard_v1_key(keyboard, time, key, WL_KEYBOARD_KEY_STATE_PRESSED);
    last_key_time++;
    zwp_virtual_keyboard_v1_key(keyboard, last_key_time, key, WL_KEYBOARD_KEY_STATE_RELEASED);
}

static int deadline_timeout_ms(uint64_t now, uint64_t deadline) {
    if (now >= deadline) return 0;
    uint64_t remaining = deadline - now;
    return remaining > INT_MAX ? INT_MAX : (int)remaining;
}

static int wait_fd_until(int fd, short events, uint64_t deadline) {
    struct pollfd pfd = {.fd = fd, .events = events};
    for (;;) {
        int timeout_ms = deadline_timeout_ms(monotonic_ms(), deadline);
        if (timeout_ms == 0) return -1;
        int result = poll(&pfd, 1, timeout_ms);
        if (result < 0 && errno == EINTR) continue;
        if (result <= 0 || (pfd.revents & (POLLERR | POLLHUP | POLLNVAL))) return -1;
        return (pfd.revents & events) ? 0 : -1;
    }
}

static int read_command(int fd, char *out, size_t cap, int timeout_ms) {
    size_t used = 0;
    uint64_t deadline = monotonic_ms() + (uint64_t)timeout_ms;
    while (used + 1 < cap && used < MAX_COMMAND) {
        if (wait_fd_until(fd, POLLIN, deadline) != 0) return -1;
        char byte;
        ssize_t n = recv(fd, &byte, 1, 0);
        if (n <= 0) return -1;
        if (byte == '\n') {
            out[used] = '\0';
            return 0;
        }
        if (byte < 'A' || byte > 'Z') return -1;
        out[used++] = byte;
    }
    return -1;
}

static void reply(int fd, const char *text) {
    size_t len = strlen(text);
    if (len > 32) fail("internal response exceeds bound");
    if (send(fd, text, len, MSG_NOSIGNAL) != (ssize_t)len) fail("control response failed");
}

static enum fixed_command parse_command(const char *command) {
    if (strcmp(command, "COMPOSE") == 0) return COMMAND_COMPOSE;
    if (strcmp(command, "COMMIT") == 0) return COMMAND_COMMIT;
    if (strcmp(command, "QUIT") == 0) return COMMAND_QUIT;
    return COMMAND_INVALID;
}

static void ignore_signal(int signal_number) { (void)signal_number; }

static enum input_state transition(enum input_state state, enum fixed_command command) {
    if (command == COMMAND_QUIT) return INPUT_QUIT;
    if (state == INPUT_READY && command == COMMAND_COMPOSE) return INPUT_COMPOSED;
    if (state == INPUT_COMPOSED && command == COMMAND_COMMIT) return INPUT_COMMITTED;
    return state;
}

static int command_allowed(enum input_state state, enum fixed_command command) {
    return command == COMMAND_QUIT ||
        (state == INPUT_READY && command == COMMAND_COMPOSE) ||
        (state == INPUT_COMPOSED && command == COMMAND_COMMIT);
}

static int selftest(void) {
    static const uint32_t compose[] = {KEY_N, KEY_I, KEY_H, KEY_A, KEY_O};
    if (sizeof(compose) / sizeof(compose[0]) != 5 || KEY_SPACE != 57 ||
        parse_command("COMPOSE") != COMMAND_COMPOSE ||
        parse_command("COMMIT") != COMMAND_COMMIT ||
        parse_command("QUIT") != COMMAND_QUIT ||
        parse_command("COMPOSE anything") != COMMAND_INVALID ||
        parse_command("nihao") != COMMAND_INVALID ||
        parse_command("compose") != COMMAND_INVALID ||
        deadline_timeout_ms(UINT32_MAX - 5u, (uint64_t)UINT32_MAX + 15u) != 20 ||
        deadline_timeout_ms(900, 900) != 0) return 1;
    if (!command_allowed(INPUT_READY, COMMAND_COMPOSE) ||
        transition(INPUT_READY, COMMAND_COMPOSE) != INPUT_COMPOSED ||
        command_allowed(INPUT_READY, COMMAND_COMMIT) ||
        command_allowed(INPUT_COMPOSED, COMMAND_COMPOSE) ||
        !command_allowed(INPUT_COMPOSED, COMMAND_COMMIT) ||
        transition(INPUT_COMPOSED, COMMAND_COMMIT) != INPUT_COMMITTED ||
        command_allowed(INPUT_COMMITTED, COMMAND_COMPOSE) ||
        command_allowed(INPUT_COMMITTED, COMMAND_COMMIT) ||
        !command_allowed(INPUT_READY, COMMAND_QUIT) ||
        !command_allowed(INPUT_COMPOSED, COMMAND_QUIT) ||
        !command_allowed(INPUT_COMMITTED, COMMAND_QUIT) ||
        transition(INPUT_READY, COMMAND_QUIT) != INPUT_QUIT) return 1;
    int sockets[2];
    char command[MAX_COMMAND + 1];
    if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sockets) != 0) return 1;
    int ok = send(sockets[0], "COMPOSE\n", 8, MSG_NOSIGNAL) == 8 &&
             read_command(sockets[1], command, sizeof(command), 100) == 0 &&
             parse_command(command) == COMMAND_COMPOSE;
    if (send(sockets[0], "BOGUS\n", 6, MSG_NOSIGNAL) != 6 ||
        read_command(sockets[1], command, sizeof(command), 100) != 0 ||
        parse_command(command) != COMMAND_INVALID) ok = 0;
    if (send(sockets[0], "partial", 7, MSG_NOSIGNAL) != 7 ||
        read_command(sockets[1], command, sizeof(command), 25) == 0) ok = 0;
    close(sockets[0]); close(sockets[1]);
    if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sockets) != 0) return 1;
    struct sigaction action = {.sa_handler = ignore_signal};
    struct sigaction old_action;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGALRM, &action, &old_action) != 0) return 1;
    struct itimerval timer = {.it_interval = {.tv_sec = 0, .tv_usec = 2000},
                              .it_value = {.tv_sec = 0, .tv_usec = 2000}};
    if (setitimer(ITIMER_REAL, &timer, NULL) != 0) return 1;
    uint64_t interrupt_start = monotonic_ms();
    int interrupted_read = read_command(sockets[1], command, sizeof(command), 60);
    uint64_t interrupted_elapsed = monotonic_ms() - interrupt_start;
    struct itimerval stopped = {0};
    setitimer(ITIMER_REAL, &stopped, NULL);
    sigaction(SIGALRM, &old_action, NULL);
    close(sockets[0]); close(sockets[1]);
    if (interrupted_read == 0 || interrupted_elapsed < 40 || interrupted_elapsed > 500) ok = 0;
    if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sockets) != 0) return 1;
    char oversized[MAX_COMMAND + 2];
    memset(oversized, 'A', sizeof(oversized));
    oversized[sizeof(oversized) - 1] = '\n';
    if (send(sockets[0], oversized, sizeof(oversized), MSG_NOSIGNAL) != (ssize_t)sizeof(oversized) ||
        read_command(sockets[1], command, sizeof(command), 100) == 0) ok = 0;
    close(sockets[0]); close(sockets[1]);
    if (!ok) return 1;
    puts("fixed-sequence-selftest: monotonic deadline; COMPOSE once, then COMMIT once; QUIT anytime");
    return 0;
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "--selftest") == 0) return selftest();
    if (argc != 3 || strcmp(argv[1], "--sway-pid") != 0) fail("usage mismatch");
    char *end = NULL;
    long sway_pid = strtol(argv[2], &end, 10);
    if (!end || *end || sway_pid < 2 || sway_pid > INT32_MAX) fail("invalid expected Sway PID");
    const char *runtime = getenv("XDG_RUNTIME_DIR");
    const char *display_name = getenv("WAYLAND_DISPLAY");
    if (!runtime || strcmp(runtime, "/run/pigade-ime") != 0 ||
        !display_name || strcmp(display_name, "wayland-1") != 0)
        fail("private Wayland environment does not match fixed endpoint");
    const char *xkb_root = getenv("XKB_CONFIG_ROOT");
    if (!xkb_root || strcmp(xkb_root, "/pigade/fcitx-core/usr/share/xkeyboard-config-2") != 0)
        fail("fixed keymap data root is not the verified signed XKB payload");
    const char *xkb_files[] = {"/keycodes/evdev", "/symbols/us", "/rules/evdev"};
    for (size_t i = 0; i < sizeof(xkb_files) / sizeof(xkb_files[0]); i++) {
        char path[256];
        int n = snprintf(path, sizeof(path), "%s%s", xkb_root, xkb_files[i]);
        struct stat info;
        if (n <= 0 || (size_t)n >= sizeof(path) || stat(path, &info) != 0 ||
            !S_ISREG(info.st_mode)) fail("verified signed XKB keymap file is missing");
    }

    struct wl_display *display = wl_display_connect(NULL);
    if (!display) fail("cannot connect to private Wayland display");
    check_peer(display, (pid_t)sway_pid);
    struct wl_registry *registry = wl_display_get_registry(display);
    struct globals globals = {0};
    wl_registry_add_listener(registry, &registry_listener, &globals);
    if (wl_display_roundtrip(display) < 0 || globals.seats != 1 || globals.managers != 1 ||
        !globals.seat || !globals.manager) fail("expected exactly one private seat and virtual-keyboard manager");

    struct xkb_context *xkb = xkb_context_new(XKB_CONTEXT_NO_FLAGS);
    if (!xkb) fail("cannot initialize fixed XKB context");
    const struct xkb_rule_names names = {
        .rules = "evdev", .model = "pc105", .layout = "us", .variant = "", .options = ""
    };
    struct xkb_keymap *keymap = xkb_keymap_new_from_names(xkb, &names, XKB_KEYMAP_COMPILE_NO_FLAGS);
    if (!keymap) fail("cannot compile fixed US keymap");
    char *keymap_text = xkb_keymap_get_as_string(keymap, XKB_KEYMAP_FORMAT_TEXT_V1);
    if (!keymap_text) fail("cannot serialize fixed US keymap");
    size_t keymap_size = strlen(keymap_text) + 1;
    if (keymap_size > 1024 * 1024) fail("fixed keymap exceeds size bound");
    int map_fd = keymap_fd(keymap_text, keymap_size);
    struct zwp_virtual_keyboard_v1 *keyboard =
        zwp_virtual_keyboard_manager_v1_create_virtual_keyboard(globals.manager, globals.seat);
    if (!keyboard) fail("private compositor refused virtual keyboard creation");
    zwp_virtual_keyboard_v1_keymap(keyboard, WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1,
                                   map_fd, (uint32_t)keymap_size);
    close(map_fd);
    zwp_virtual_keyboard_v1_modifiers(keyboard, 0, 0, 0, 0);
    if (wl_display_roundtrip(display) < 0) fail("virtual keyboard setup roundtrip failed");

    int control = make_control_socket();
    reply(control, "READY\n");
    enum input_state state = INPUT_READY;
    for (;;) {
        char command[ MAX_COMMAND + 1 ];
        if (read_command(control, command, sizeof(command), CONTROL_TIMEOUT_MS) != 0)
            fail("bounded command read failed");
        enum fixed_command parsed = parse_command(command);
        if (!command_allowed(state, parsed)) fail("fixed command violates one-shot sequence");
        state = transition(state, parsed);
        if (parsed == COMMAND_COMPOSE) {
            static const uint32_t keys[] = {KEY_N, KEY_I, KEY_H, KEY_A, KEY_O};
            for (size_t i = 0; i < sizeof(keys) / sizeof(keys[0]); i++) send_key(keyboard, keys[i]);
            if (wl_display_roundtrip(display) < 0) fail("COMPOSE flush failed");
            reply(control, "COMPOSED\n");
        } else if (parsed == COMMAND_COMMIT) {
            send_key(keyboard, KEY_SPACE);
            if (wl_display_roundtrip(display) < 0) fail("COMMIT flush failed");
            reply(control, "COMMITTED\n");
        } else if (parsed == COMMAND_QUIT) {
            zwp_virtual_keyboard_v1_destroy(keyboard);
            wl_display_roundtrip(display);
            reply(control, "QUIT\n");
            close(control);
            xkb_keymap_unref(keymap); xkb_context_unref(xkb); free(keymap_text);
            wl_seat_destroy(globals.seat);
            zwp_virtual_keyboard_manager_v1_destroy(globals.manager);
            wl_registry_destroy(registry);
            wl_display_disconnect(display);
            return 0;
        } else {
            fail("unknown fixed command");
        }
    }
}
