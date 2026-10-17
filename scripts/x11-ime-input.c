/* Fixed-sequence X11/XTEST helper for a private PigADE IME fixture.
 * It intentionally has no text/key arguments: only fixed compose/commit,
 * EN/RU/CLI, workspace-context and generation-history sequences can synthesize input,
 * after private-server ownership checks. The latency stage uses50 fixed
 * alphabet keys and private pixel readback. No arbitrary key/text arguments.
 */
#define _GNU_SOURCE
#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/XKBlib.h>
#include <X11/keysym.h>
#include <X11/extensions/XTest.h>
#include <xcb/xcb.h>
#include <xcb/res.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define MAX_W 1280
#define MAX_H 800
#define MAX_PIXELS (MAX_W * MAX_H)

typedef struct {
    const char *stage, *display, *boot, *output;
    pid_t xvfb, fcitx, app;
    unsigned long long xvfb_start, fcitx_start, app_start, socket_inode;
} Args;

static void fail(const char *message) { fprintf(stderr, "x11-ime-input: %s\n", message); exit(2); }

static unsigned long long parse_u64(const char *s) {
    char *end = NULL; errno = 0; unsigned long long n = strtoull(s, &end, 10);
    if (errno || !s[0] || !end || *end) fail("invalid numeric argument");
    return n;
}

static unsigned long long self_process_start(void);

static Args parse_args(int argc, char **argv) {
    Args a = {0};
    for (int i = 1; i < argc; i += 2) {
        if (i + 1 >= argc) fail("missing argument value");
        const char *k = argv[i], *v = argv[i + 1];
        if (!strcmp(k, "--stage")) a.stage = v;
        else if (!strcmp(k, "--display")) a.display = v;
        else if (!strcmp(k, "--boot-id")) a.boot = v;
        else if (!strcmp(k, "--output")) a.output = v;
        else if (!strcmp(k, "--xvfb-pid")) a.xvfb = (pid_t)parse_u64(v);
        else if (!strcmp(k, "--xvfb-start")) a.xvfb_start = parse_u64(v);
        else if (!strcmp(k, "--xvfb-socket-inode")) a.socket_inode = parse_u64(v);
        else if (!strcmp(k, "--fcitx-pid")) a.fcitx = (pid_t)parse_u64(v);
        else if (!strcmp(k, "--fcitx-start")) a.fcitx_start = parse_u64(v);
        else if (!strcmp(k, "--app-pid")) a.app = (pid_t)parse_u64(v);
        else if (!strcmp(k, "--app-start")) a.app_start = parse_u64(v);
        else fail("unknown argument");
    }
    if (!a.stage || !a.display || !a.boot || !a.xvfb || !a.xvfb_start || !a.socket_inode ||
        !a.fcitx || !a.fcitx_start) fail("missing required identity argument");
    if (strcmp(a.stage, "preflight") && strcmp(a.stage, "probe-window") &&
        strcmp(a.stage, "focus") && strcmp(a.stage, "compose") &&
        strcmp(a.stage, "commit") && strcmp(a.stage, "capture") &&
        strcmp(a.stage, "keyboard") && strcmp(a.stage, "latency") &&
        strcmp(a.stage, "context-open") && strcmp(a.stage, "context-edit") &&
        strcmp(a.stage, "context-save") && strcmp(a.stage, "memory-open") &&
        strcmp(a.stage, "memory-select") && strcmp(a.stage, "generations-open") &&
        strcmp(a.stage, "generations-select-old") && strcmp(a.stage, "generations-follow") &&
        strcmp(a.stage, "resume-open") && strcmp(a.stage, "resume-confirm") &&
        strcmp(a.stage, "history-open") && strcmp(a.stage, "history-search") &&
        strcmp(a.stage, "history-readonly"))
        fail("unsupported stage");
    if (strcmp(a.stage, "preflight") && (!a.app || !a.app_start)) fail("app identity required");
    if (!strcmp(a.stage, "capture") && (!a.output || strncmp(a.output, "/artifacts/", 11)))
        fail("capture path must be under /artifacts");
    if (strncmp(a.display, ":", 1) || strspn(a.display + 1, "0123456789") != strlen(a.display + 1))
        fail("only a local numeric private display is accepted");
    return a;
}

static void read_file(const char *path, char *out, size_t cap) {
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0) fail("cannot open identity file");
    ssize_t n = read(fd, out, cap - 1); close(fd);
    if (n <= 0 || (size_t)n >= cap - 1) fail("identity file is empty or oversized");
    out[n] = 0;
}

static unsigned long long proc_start(pid_t pid) {
    char path[64], line[4096]; snprintf(path, sizeof(path), "/proc/%ld/stat", (long)pid);
    read_file(path, line, sizeof(line));
    char *close = strrchr(line, ')'); if (!close || close[1] != ' ') fail("malformed process stat");
    char *p = close + 2; /* field 3 (state) */
    for (int field = 3; field < 22; ++field) {
        while (*p == ' ') ++p;
        char *end = strchr(p, ' ');
        if (!end) fail("short process stat");
        if (field == 22) break;
        p = end + 1;
    }
    while (*p == ' ') ++p;
    char *end = strchr(p, ' ');
    if (!end) end = p + strlen(p);
    char token[64]; size_t length = (size_t)(end - p);
    if (!length || length >= sizeof(token)) fail("invalid process start time");
    memcpy(token, p, length); token[length] = 0;
    return parse_u64(token);
}

static unsigned long long self_process_start(void) { return proc_start(getpid()); }

static void verify_identity(pid_t pid, unsigned long long start, const char *boot) {
    char current[128]; read_file("/proc/sys/kernel/random/boot_id", current, sizeof(current));
    current[strcspn(current, "\r\n")] = 0;
    if (strcmp(current, boot) || proc_start(pid) != start) fail("owned process identity changed");
}

static xcb_connection_t *xcb_open_checked(Display *dpy, pid_t expected_peer) {
    xcb_connection_t *c = xcb_connect(DisplayString(dpy), NULL);
    if (!c || xcb_connection_has_error(c)) fail("cannot open local XCB connection");
    int fd = xcb_get_file_descriptor(c); struct ucred cred; socklen_t n = sizeof(cred);
    if (getsockopt(fd, SOL_SOCKET, SO_PEERCRED, &cred, &n) || cred.pid != expected_peer ||
        cred.uid != getuid()) fail("XCB peer is not the owned Xvfb");
    const xcb_query_extension_reply_t *res = xcb_get_extension_data(c, &xcb_res_id);
    if (!res || !res->present) fail("XRes extension unavailable");
    return c;
}

static pid_t xres_pid(xcb_connection_t *c, uint32_t xid) {
    xcb_res_client_id_spec_t spec = { .client = xid,
        .mask = XCB_RES_CLIENT_ID_MASK_CLIENT_XID | XCB_RES_CLIENT_ID_MASK_LOCAL_CLIENT_PID };
    xcb_res_query_client_ids_cookie_t cookie = xcb_res_query_client_ids(c, 1, &spec);
    xcb_generic_error_t *error = NULL;
    xcb_res_query_client_ids_reply_t *reply = xcb_res_query_client_ids_reply(c, cookie, &error);
    if (!reply || error) { free(reply); free(error); fail("XRes client PID query failed"); }
    pid_t result = -1;
    xcb_res_client_id_value_iterator_t it = xcb_res_query_client_ids_ids_iterator(reply);
    for (; it.rem; xcb_res_client_id_value_next(&it)) {
        if (it.data->spec.mask & XCB_RES_CLIENT_ID_MASK_LOCAL_CLIENT_PID) {
            /* Wire length is 4 bytes for one PID; libxcb's generated accessor
             * reports the decoded CARD32 element count (1). */
            if (it.data->length != sizeof(uint32_t) ||
                xcb_res_client_id_value_value_length(it.data) != 1 || result != -1) {
                free(reply); fail("ambiguous XRes PID");
            }
            result = (pid_t)xcb_res_client_id_value_value(it.data)[0];
        }
    }
    free(reply);
    if (result < 0) fail("XRes local PID unavailable");
    return result;
}

static Window find_app_window(Display *dpy, xcb_connection_t *c, pid_t app) {
    Window root = DefaultRootWindow(dpy), parent, *children = NULL;
    unsigned int count = 0;
    if (!XQueryTree(dpy, root, &root, &parent, &children, &count)) fail("cannot query X tree");
    Atom pid_atom = XInternAtom(dpy, "_NET_WM_PID", True);
    if (pid_atom == None) fail("_NET_WM_PID atom unavailable");
    Window found = None;
    for (unsigned int i = 0; i < count; ++i) {
        XWindowAttributes attr;
        if (!XGetWindowAttributes(dpy, children[i], &attr) || attr.map_state != IsViewable) continue;
        Atom type; int format; unsigned long nitems, after; unsigned char *value = NULL;
        int status = XGetWindowProperty(dpy, children[i], pid_atom, 0, 1, False, XA_CARDINAL,
                                        &type, &format, &nitems, &after, &value);
        pid_t prop_pid = -1;
        if (status == Success && type == XA_CARDINAL && format == 32 && nitems == 1 && value)
            prop_pid = (pid_t)*(unsigned long *)value;
        if (value) XFree(value);
        if (prop_pid == app) {
            if (found != None || xres_pid(c, children[i]) != app) { XFree(children); fail("app window PID is ambiguous"); }
            found = children[i];
        }
    }
    if (children) XFree(children);
    return found;
}

static Window require_app_window(Window window) {
    if (window == None) fail("verified app window is absent");
    return window;
}

static void verify_xim_owner(Display *dpy, xcb_connection_t *c, pid_t expected) {
    Atom property = XInternAtom(dpy, "XIM_SERVERS", True);
    if (property == None) fail("XIM_SERVERS is absent");
    Atom actual; int format; unsigned long count, after; unsigned char *raw = NULL;
    if (XGetWindowProperty(dpy, DefaultRootWindow(dpy), property, 0, 64, False, XA_ATOM,
                           &actual, &format, &count, &after, &raw) != Success ||
        actual != XA_ATOM || format != 32 || after != 0 || count == 0 || !raw)
        fail("invalid XIM_SERVERS property");
    Atom *atoms = (Atom *)raw; int active = 0; int found = 0;
    for (unsigned long i = 0; i < count; ++i) {
        Window owner = XGetSelectionOwner(dpy, atoms[i]);
        if (owner == None) continue;
        ++active;
        char *name = XGetAtomName(dpy, atoms[i]);
        pid_t pid = xres_pid(c, owner);
        if (name && !strcmp(name, "@server=fcitx") && pid == expected) found = 1;
        if (name) XFree(name);
        if (pid != expected) { XFree(raw); fail("unexpected active XIM owner"); }
    }
    XFree(raw);
    if (active != 1 || !found) fail("private Fcitx XIM selection is not uniquely owned");
}

static void verify_server(const Args *a, Display **out, xcb_connection_t **out_c) {
    verify_identity(a->xvfb, a->xvfb_start, a->boot);
    verify_identity(a->fcitx, a->fcitx_start, a->boot);
    char socket_path[128]; snprintf(socket_path, sizeof(socket_path), "/tmp/.X11-unix/X%s", a->display + 1);
    struct stat st;
    if (lstat(socket_path, &st) || !S_ISSOCK(st.st_mode) || st.st_uid != getuid() ||
        (unsigned long long)st.st_ino != a->socket_inode) fail("private X socket identity mismatch");
    Display *dpy = XOpenDisplay(a->display);
    if (!dpy) fail("cannot connect to selected X display");
    int fd = ConnectionNumber(dpy); struct ucred cred; socklen_t n = sizeof(cred);
    if (getsockopt(fd, SOL_SOCKET, SO_PEERCRED, &cred, &n) || cred.pid != a->xvfb || cred.uid != getuid())
        fail("Xlib peer is not the owned Xvfb");
    int event, error, major, minor;
    if (!XTestQueryExtension(dpy, &event, &error, &major, &minor)) fail("XTEST extension is unavailable");
    xcb_connection_t *c = xcb_open_checked(dpy, a->xvfb);
    verify_xim_owner(dpy, c, a->fcitx);
    *out = dpy; *out_c = c;
}

static void key(Display *dpy, KeySym sym) {
    KeyCode code = XKeysymToKeycode(dpy, sym);
    if (!code || !XTestFakeKeyEvent(dpy, code, True, CurrentTime)) fail("XTEST key press failed");
    XFlush(dpy); struct timespec delay = { .tv_sec = 0, .tv_nsec = 45000000 }; nanosleep(&delay, NULL);
    if (!XTestFakeKeyEvent(dpy, code, False, CurrentTime)) fail("XTEST key release failed");
    XFlush(dpy); nanosleep(&delay, NULL);
}

/* Both group changes and modifier injection stay on the verified private Xvfb. */
static void keyboard_group(Display *dpy, unsigned int group) {
    XkbStateRec state;
    if (!XkbLockGroup(dpy, XkbUseCoreKbd, group) ||
        XkbGetState(dpy, XkbUseCoreKbd, &state) != Success ||
        state.group != group || state.mods != 0)
        fail("private keyboard group/modifier state differs from fixture");
    struct timespec delay = { .tv_sec = 0, .tv_nsec = 100000000 };
    nanosleep(&delay, NULL);
}

static void modified_key(Display *dpy, KeySym modifier, KeySym sym) {
    KeyCode code = XKeysymToKeycode(dpy, modifier);
    if (!code || !XTestFakeKeyEvent(dpy, code, True, CurrentTime))
        fail("private modifier press failed");
    XSync(dpy, False);
    key(dpy, sym);
    if (!XTestFakeKeyEvent(dpy, code, False, CurrentTime))
        fail("private modifier release failed");
    XSync(dpy, False);
}

static void context_open_sequence(Display *dpy) {
    KeyCode control = XKeysymToKeycode(dpy, XK_Control_L);
    KeyCode shift = XKeysymToKeycode(dpy, XK_Shift_L);
    if (!control || !shift || !XTestFakeKeyEvent(dpy, control, True, CurrentTime) ||
        !XTestFakeKeyEvent(dpy, shift, True, CurrentTime)) fail("context shortcut press failed");
    key(dpy, XK_space);
    if (!XTestFakeKeyEvent(dpy, shift, False, CurrentTime) ||
        !XTestFakeKeyEvent(dpy, control, False, CurrentTime)) fail("context shortcut release failed");
    key(dpy, XK_e);
}

static void memory_open_sequence(Display *dpy) {
    KeyCode control = XKeysymToKeycode(dpy, XK_Control_L);
    KeyCode shift = XKeysymToKeycode(dpy, XK_Shift_L);
    if (!control || !shift || !XTestFakeKeyEvent(dpy, control, True, CurrentTime) ||
        !XTestFakeKeyEvent(dpy, shift, True, CurrentTime)) fail("memory review shortcut press failed");
    key(dpy, XK_space);
    if (!XTestFakeKeyEvent(dpy, shift, False, CurrentTime) ||
        !XTestFakeKeyEvent(dpy, control, False, CurrentTime)) fail("memory review shortcut release failed");
    key(dpy, XK_v);
}

static void memory_select_sequence(Display *dpy) {
    key(dpy, XK_Tab);
    key(dpy, XK_Tab);
    key(dpy, XK_2);
    modified_key(dpy, XK_Control_L, XK_r);
}

static void generations_open_sequence(Display *dpy) {
    KeyCode control = XKeysymToKeycode(dpy, XK_Control_L);
    KeyCode shift = XKeysymToKeycode(dpy, XK_Shift_L);
    if (!control || !shift || !XTestFakeKeyEvent(dpy, control, True, CurrentTime) ||
        !XTestFakeKeyEvent(dpy, shift, True, CurrentTime)) fail("generation picker shortcut press failed");
    key(dpy, XK_space);
    if (!XTestFakeKeyEvent(dpy, shift, False, CurrentTime) ||
        !XTestFakeKeyEvent(dpy, control, False, CurrentTime)) fail("generation picker shortcut release failed");
    key(dpy, XK_o);
}

static void history_open_sequence(Display *dpy) {
    KeyCode control = XKeysymToKeycode(dpy, XK_Control_L);
    KeyCode shift = XKeysymToKeycode(dpy, XK_Shift_L);
    if (!control || !shift || !XTestFakeKeyEvent(dpy, control, True, CurrentTime) ||
        !XTestFakeKeyEvent(dpy, shift, True, CurrentTime)) fail("history shortcut press failed");
    key(dpy, XK_space);
    if (!XTestFakeKeyEvent(dpy, shift, False, CurrentTime) ||
        !XTestFakeKeyEvent(dpy, control, False, CurrentTime)) fail("history shortcut release failed");
    key(dpy, XK_i);
}

static void generations_select_old_sequence(Display *dpy) {
    key(dpy, XK_2);
    key(dpy, XK_Return);
}

static void generations_follow_sequence(Display *dpy) {
    const KeySym current[] = { XK_c, XK_u, XK_r, XK_r, XK_e, XK_n, XK_t };
    for (size_t i = 0; i < sizeof(current) / sizeof(current[0]); ++i) key(dpy, current[i]);
    key(dpy, XK_Return);
}

static void resume_open_sequence(Display *dpy) {
    KeyCode control = XKeysymToKeycode(dpy, XK_Control_L);
    KeyCode shift = XKeysymToKeycode(dpy, XK_Shift_L);
    if (!control || !shift || !XTestFakeKeyEvent(dpy, control, True, CurrentTime) ||
        !XTestFakeKeyEvent(dpy, shift, True, CurrentTime)) fail("resume palette shortcut press failed");
    key(dpy, XK_space);
    if (!XTestFakeKeyEvent(dpy, shift, False, CurrentTime) ||
        !XTestFakeKeyEvent(dpy, control, False, CurrentTime)) fail("resume palette shortcut release failed");
    key(dpy, XK_p);
    const KeySym query[] = { XK_r, XK_e, XK_s, XK_u, XK_m, XK_e };
    for (size_t i = 0; i < sizeof(query) / sizeof(query[0]); ++i) key(dpy, query[i]);
    key(dpy, XK_Return);
}

static void context_edit_sequence(Display *dpy) {
    key(dpy, XK_a); key(dpy, XK_b); key(dpy, XK_Left);
    modified_key(dpy, XK_Shift_L, XK_Left); key(dpy, XK_x);
    modified_key(dpy, XK_Control_L, XK_z);
    KeyCode control = XKeysymToKeycode(dpy, XK_Control_L);
    KeyCode shift = XKeysymToKeycode(dpy, XK_Shift_L);
    if (!control || !shift || !XTestFakeKeyEvent(dpy, control, True, CurrentTime) ||
        !XTestFakeKeyEvent(dpy, shift, True, CurrentTime)) fail("redo shortcut press failed");
    key(dpy, XK_z);
    if (!XTestFakeKeyEvent(dpy, shift, False, CurrentTime) ||
        !XTestFakeKeyEvent(dpy, control, False, CurrentTime)) fail("redo shortcut release failed");
}

static void keyboard_sequence(Display *dpy) {
    char pressed[32];
    XQueryKeymap(dpy, pressed);
    for (size_t i = 0; i < sizeof(pressed); ++i)
        if (pressed[i]) fail("private keyboard has an unexpectedly held key");
    keyboard_group(dpy, 0);
    key(dpy, XK_a); key(dpy, XK_b); key(dpy, XK_1);
    modified_key(dpy, XK_Shift_L, XK_z);
    const KeySym controls[] = { XK_c, XK_d, XK_z, XK_r, XK_l, XK_a };
    for (size_t i = 0; i < sizeof(controls) / sizeof(controls[0]); ++i)
        modified_key(dpy, XK_Control_L, controls[i]);
    modified_key(dpy, XK_Alt_L, XK_x);
    key(dpy, XK_Left); key(dpy, XK_Home); key(dpy, XK_Return);
    keyboard_group(dpy, 1);
    const KeySym russian[] = { XK_Cyrillic_pe, XK_Cyrillic_er, XK_Cyrillic_i,
                               XK_Cyrillic_ve, XK_Cyrillic_ie, XK_Cyrillic_te };
    for (size_t i = 0; i < sizeof(russian) / sizeof(russian[0]); ++i)
        key(dpy, russian[i]);
    keyboard_group(dpy, 0);
    XQueryKeymap(dpy, pressed);
    for (size_t i = 0; i < sizeof(pressed); ++i)
        if (pressed[i]) fail("private keyboard retained a held key after fixture");
}

static Window verify_app_focus_target(const Args *a, Display *dpy, xcb_connection_t *c, int set_focus) {
    verify_identity(a->app, a->app_start, a->boot);
    Window w = find_app_window(dpy, c, a->app);
    require_app_window(w);
    XWindowAttributes attr;
    if (!XGetWindowAttributes(dpy, w, &attr) || attr.map_state != IsViewable) fail("verified app window is not viewable");
    if (set_focus) {
        XSetInputFocus(dpy, w, RevertToParent, CurrentTime); XSync(dpy, False);
    }
    Window focused; int revert;
    XGetInputFocus(dpy, &focused, &revert);
    if (focused != w) fail("focus is not the verified private app window");
    return w;
}

static unsigned int component(unsigned long pixel, unsigned long mask) {
    if (!mask) return 0;
    int shift = 0; while (((mask >> shift) & 1ul) == 0) ++shift;
    unsigned long max = mask >> shift, value = (pixel & mask) >> shift;
    return (unsigned int)((value * 255ul + max / 2ul) / max);
}

static void capture(Display *dpy, const char *path) {
    Window root = DefaultRootWindow(dpy); XWindowAttributes attr;
    if (!XGetWindowAttributes(dpy, root, &attr) || attr.width <= 0 || attr.height <= 0 ||
        attr.width > MAX_W || attr.height > MAX_H || attr.width * attr.height > MAX_PIXELS)
        fail("private capture geometry exceeds bound");
    XImage *image = XGetImage(dpy, root, 0, 0, (unsigned int)attr.width, (unsigned int)attr.height,
                              AllPlanes, ZPixmap);
    if (!image) fail("cannot capture private X root");
    int fd = open(path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0600);
    if (fd < 0) { XDestroyImage(image); fail("cannot create bounded capture"); }
    char header[64]; int h = snprintf(header, sizeof(header), "P6\n%d %d\n255\n", attr.width, attr.height);
    if (write(fd, header, (size_t)h) != h) fail("cannot write PPM header");
    unsigned char row[MAX_W * 3];
    for (int y = 0; y < attr.height; ++y) {
        for (int x = 0; x < attr.width; ++x) {
            unsigned long p = XGetPixel(image, x, y);
            row[x * 3] = (unsigned char)component(p, image->red_mask);
            row[x * 3 + 1] = (unsigned char)component(p, image->green_mask);
            row[x * 3 + 2] = (unsigned char)component(p, image->blue_mask);
        }
        if (write(fd, row, (size_t)attr.width * 3) != (ssize_t)attr.width * 3) fail("short PPM capture write");
    }
    fsync(fd); close(fd); XDestroyImage(image);
}

static uint64_t monotonic_ns(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now)) fail("monotonic clock unavailable");
    return (uint64_t)now.tv_sec * UINT64_C(1000000000) + (uint64_t)now.tv_nsec;
}

static void latency_failure_json(unsigned int failed_index, const uint64_t *samples,
                                 const unsigned int expected[3], const unsigned int observed[3],
                                 uint64_t elapsed_ns, const char *reason) {
    printf("{\"version\":1,\"stage\":\"latency\","
           "\"scope\":\"XTEST-submit to private root pixel readback\","
           "\"pixel\":[20,120],\"sample_count\":%u,\"samples_ns\":[",
           failed_index);
    for (unsigned int i = 0; i < failed_index; ++i)
        printf("%s%" PRIu64, i ? "," : "", samples[i]);
    printf("],\"failed_index\":%u,\"expected_rgb\":[%u,%u,%u],"
           "\"observed_rgb\":[%u,%u,%u],\"elapsed_ns\":%" PRIu64
           ",\"reason\":\"%s\"}\n",
           failed_index, expected[0], expected[1], expected[2],
           observed[0], observed[1], observed[2], elapsed_ns, reason);
    fflush(stdout);
}

static void latency_fail(unsigned int index, const uint64_t *samples,
                         uint64_t elapsed_ns, unsigned int observed_r,
                         unsigned int observed_g, unsigned int observed_b,
                         const char *reason, const char *message) {
    const unsigned int expected[3] = {17 + 4 * index, 239, 27};
    const unsigned int observed[3] = {observed_r, observed_g, observed_b};
    latency_failure_json(index, samples, expected, observed, elapsed_ns, reason);
    fail(message);
}

/* Fixed50 alphabet keys, each observed through a causally unique flat cell color.
 * This measures private software X11 pixels, not physical monitor scanout. */
static void latency_sequence(const Args *a, Display *dpy, xcb_connection_t *c) {
    uint64_t samples[50] = {0};
    keyboard_group(dpy, 0);
    for (unsigned int index = 0; index < 50; ++index) {
        verify_app_focus_target(a, dpy, c, 0);
        KeyCode code = XKeysymToKeycode(dpy, XK_a + index % 26);
        if (!code) latency_fail(index, samples, 1,
                                0, 0, 0, "keycode_unavailable", "fixed alphabet key unavailable");
        uint64_t start = monotonic_ns();
        if (!XTestFakeKeyEvent(dpy, code, True, CurrentTime) ||
            !XTestFakeKeyEvent(dpy, code, False, CurrentTime))
            latency_fail(index, samples, monotonic_ns() - start,
                         0, 0, 0, "key_submission_failed", "latency key submission failed; no retry");
        XFlush(dpy);
        while (1) {
            XImage *pixel = XGetImage(dpy, DefaultRootWindow(dpy), 20, 120, 1, 1, AllPlanes, ZPixmap);
            if (!pixel) latency_fail(index, samples, monotonic_ns() - start,
                                     0, 0, 0, "pixel_readback_failed", "private pixel readback failed");
            unsigned long value = XGetPixel(pixel, 0, 0);
            unsigned int observed[3] = {component(value, pixel->red_mask),
                                        component(value, pixel->green_mask),
                                        component(value, pixel->blue_mask)};
            unsigned int expected[3] = {17 + 4 * index, 239, 27};
            int matched = observed[0] == expected[0] && observed[1] == expected[1] &&
                          observed[2] == expected[2];
            XDestroyImage(pixel);
            uint64_t elapsed = monotonic_ns() - start;
            if (elapsed > UINT64_C(1000000000))
                latency_fail(index, samples, elapsed, observed[0], observed[1], observed[2],
                             "color_deadline", "fixed latency color deadline; no key retry");
            if (matched) { samples[index] = elapsed; break; }
            struct timespec pause = { .tv_sec = 0, .tv_nsec = 1000000 };
            nanosleep(&pause, NULL);
        }
    }
    printf("{\"stage\":\"latency\",\"scope\":\"XTEST-submit to private root pixel readback\","
           "\"pixel\":[20,120],\"samples_ns\":[");
    for (size_t i = 0; i < 50; ++i) printf("%s%" PRIu64, i ? "," : "", samples[i]);
    puts("]}");
}

int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "--self-test-proc")) {
        printf("%llu\n", self_process_start());
        return 0;
    }
    if (argc == 2 && !strcmp(argv[1], "--self-test-xres-length")) {
        xcb_res_client_id_value_t synthetic = {0};
        synthetic.length = sizeof(uint32_t);
        if (xcb_res_client_id_value_value_length(&synthetic) != 1) return 3;
        puts("wire_bytes=4 decoded_values=1");
        return 0;
    }
    if (argc == 2 && !strcmp(argv[1], "--self-test-window-missing")) {
        (void)require_app_window(None);
        return 0;
    }
    if (argc == 2 && !strcmp(argv[1], "--self-test-latency-diagnostic")) {
        uint64_t samples[50] = {100, 110, 120, 130, 140, 150, 160};
        latency_failure_json(7, samples, (unsigned int[]){45,239,27},
                             (unsigned int[]){41,239,27}, UINT64_C(1000100000),
                             "color_deadline");
        return 0;
    }
    Args a = parse_args(argc, argv);
    if (strcmp(getenv("DISPLAY") ? getenv("DISPLAY") : "", a.display) ||
        strcmp(getenv("XMODIFIERS") ? getenv("XMODIFIERS") : "", "@im=fcitx"))
        fail("environment is not bound to the expected private XIM display");
    Display *dpy = NULL; xcb_connection_t *c = NULL;
    verify_server(&a, &dpy, &c);
    if (!strcmp(a.stage, "preflight")) {
        printf("{\"stage\":\"preflight\",\"xvfb_pid\":%ld,\"fcitx_pid\":%ld,\"xtest\":true,\"xim_owner_verified\":true}\n", (long)a.xvfb, (long)a.fcitx);
    } else if (!strcmp(a.stage, "probe-window")) {
        Window w = find_app_window(dpy, c, a.app);
        printf("{\"stage\":\"probe-window\",\"app_pid\":%ld,\"window_verified\":%s}\n",
               (long)a.app, w == None ? "false" : "true");
    } else if (!strcmp(a.stage, "capture")) {
        verify_app_focus_target(&a, dpy, c, 0); XSync(dpy, False); capture(dpy, a.output);
        printf("{\"stage\":\"capture\",\"path\":\"%s\",\"width\":%d,\"height\":%d}\n", a.output, DisplayWidth(dpy, DefaultScreen(dpy)), DisplayHeight(dpy, DefaultScreen(dpy)));
    } else if (!strcmp(a.stage, "latency")) {
        latency_sequence(&a, dpy, c);
    } else if (!strcmp(a.stage, "focus")) {
        verify_app_focus_target(&a, dpy, c, 1);
        printf("{\"stage\":\"focus\",\"app_pid\":%ld,\"focus_verified\":true}\n", (long)a.app);
    } else {
        verify_app_focus_target(&a, dpy, c, 1);
        if (!strcmp(a.stage, "compose")) {
            const KeySym sequence[] = { XK_n, XK_i, XK_h, XK_a, XK_o };
            for (size_t i = 0; i < sizeof(sequence) / sizeof(sequence[0]); ++i) key(dpy, sequence[i]);
        } else if (!strcmp(a.stage, "context-open")) {
            context_open_sequence(dpy);
        } else if (!strcmp(a.stage, "context-edit")) {
            context_edit_sequence(dpy);
        } else if (!strcmp(a.stage, "context-save")) {
            modified_key(dpy, XK_Control_L, XK_Return);
        } else if (!strcmp(a.stage, "memory-open")) {
            memory_open_sequence(dpy);
        } else if (!strcmp(a.stage, "memory-select")) {
            memory_select_sequence(dpy);
        } else if (!strcmp(a.stage, "generations-open")) {
            generations_open_sequence(dpy);
        } else if (!strcmp(a.stage, "generations-select-old")) {
            generations_select_old_sequence(dpy);
        } else if (!strcmp(a.stage, "generations-follow")) {
            generations_follow_sequence(dpy);
        } else if (!strcmp(a.stage, "resume-open")) {
            resume_open_sequence(dpy);
        } else if (!strcmp(a.stage, "resume-confirm")) {
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "history-open")) {
            history_open_sequence(dpy);
        } else if (!strcmp(a.stage, "history-search")) {
            const KeySym query[] = { XK_r, XK_e, XK_a, XK_d, XK_y };
            for (size_t i = 0; i < sizeof(query) / sizeof(query[0]); ++i)
                modified_key(dpy, XK_Shift_L, query[i]);
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "history-readonly")) {
            key(dpy, XK_Tab); key(dpy, XK_x);
            modified_key(dpy, XK_Control_L, XK_a);
            modified_key(dpy, XK_Control_L, XK_c);
            key(dpy, XK_BackSpace);
            key(dpy, XK_Tab);
        } else if (!strcmp(a.stage, "keyboard") && strcmp(a.stage, "latency")) keyboard_sequence(dpy);
        else key(dpy, XK_space);
        const char *count = !strcmp(a.stage, "compose") ? "5" :
                            !strcmp(a.stage, "keyboard") ? "20" :
                            !strcmp(a.stage, "context-open") ? "2" :
                            !strcmp(a.stage, "memory-open") ? "2" :
                            !strcmp(a.stage, "memory-select") ? "4" :
                            !strcmp(a.stage, "generations-open") ? "2" :
                            !strcmp(a.stage, "generations-select-old") ? "2" :
                            !strcmp(a.stage, "generations-follow") ? "8" :
                            !strcmp(a.stage, "resume-open") ? "9" :
                            !strcmp(a.stage, "resume-confirm") ? "1" :
                            !strcmp(a.stage, "history-open") ? "2" :
                            !strcmp(a.stage, "history-search") ? "6" :
                            !strcmp(a.stage, "history-readonly") ? "6" :
                            !strcmp(a.stage, "context-edit") ? "7" : "1";
        printf("{\"stage\":\"%s\",\"app_pid\":%ld,\"synthetic_keystrokes\":%s}\n",
               a.stage, (long)a.app, count);
    }
    XSync(dpy, False); xcb_disconnect(c); XCloseDisplay(dpy); return 0;
}
