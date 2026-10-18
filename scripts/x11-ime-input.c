/* Fixed-sequence X11/XTEST helper for a private PigADE IME fixture.
 * It intentionally has no text/key arguments: only fixed compose/commit,
 * EN/RU/CLI, workspace-context, generation-history and template-recipe sequences can synthesize input,
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
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define MAX_W 1280
#define MAX_H 800
#define MAX_PIXELS (MAX_W * MAX_H)
static const unsigned char FIXED_CLIPBOARD_TEXT[] =
    "PIGADE_MULTILINE_A\nPIGADE_MULTILINE_B";

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
static Window verify_app_focus_target(const Args *a, Display *dpy, xcb_connection_t *c, int set_focus);
static void capture(Display *dpy, const char *path);

static void request_wm_delete(Display *dpy, Window app) {
    Atom protocols = XInternAtom(dpy, "WM_PROTOCOLS", False);
    Atom delete_window = XInternAtom(dpy, "WM_DELETE_WINDOW", False);
    Atom *supported = NULL;
    int count = 0;
    if (!XGetWMProtocols(dpy, app, &supported, &count))
        fail("verified app window does not advertise WM protocols");
    int accepts_delete = 0;
    for (int i = 0; i < count; ++i)
        if (supported[i] == delete_window) accepts_delete = 1;
    if (supported) XFree(supported);
    if (!accepts_delete) fail("verified app window does not advertise WM_DELETE_WINDOW");
    XEvent event = {0};
    event.xclient.type = ClientMessage;
    event.xclient.window = app;
    event.xclient.message_type = protocols;
    event.xclient.format = 32;
    event.xclient.data.l[0] = (long)delete_window;
    event.xclient.data.l[1] = (long)CurrentTime;
    if (!XSendEvent(dpy, app, False, NoEventMask, &event))
        fail("private WM_DELETE_WINDOW event send failed");
    XSync(dpy, False);
}

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
        strcmp(a.stage, "context-save") && strcmp(a.stage, "context-close") &&
        strcmp(a.stage, "context-close-confirm") && strcmp(a.stage, "wm-close") &&
        strcmp(a.stage, "prompts-open") && strcmp(a.stage, "prompt-new") &&
        strcmp(a.stage, "prompt-create-fields") && strcmp(a.stage, "prompt-save") &&
        strcmp(a.stage, "prompt-select-first") && strcmp(a.stage, "prompt-edit") &&
        strcmp(a.stage, "prompt-rename") && strcmp(a.stage, "prompt-save-edit") &&
        strcmp(a.stage, "prompt-close") && strcmp(a.stage, "prompt-insert-open") &&
        strcmp(a.stage, "prompt-use-first") && strcmp(a.stage, "prompt-delete") &&
        strcmp(a.stage, "prompt-delete-cancel") && strcmp(a.stage, "prompt-confirm-delete") &&
        strcmp(a.stage, "memory-open") &&
        strcmp(a.stage, "memory-select") && strcmp(a.stage, "generations-open") &&
        strcmp(a.stage, "generations-select-old") && strcmp(a.stage, "generations-follow") &&
        strcmp(a.stage, "resume-open") && strcmp(a.stage, "resume-confirm") &&
        strcmp(a.stage, "history-open") && strcmp(a.stage, "history-search") &&
        strcmp(a.stage, "history-readonly") && strcmp(a.stage, "scrollback-open") &&
        strcmp(a.stage, "scrollback-wheel") && strcmp(a.stage, "scrollback-top") &&
        strcmp(a.stage, "scrollback-newer") &&
        strcmp(a.stage, "scrollback-end") && strcmp(a.stage, "scrollback-close") &&
        strcmp(a.stage, "scrollback-probe-input") && strcmp(a.stage, "scrollback-live-input") &&
        strcmp(a.stage, "catalog-open-library") &&
        strcmp(a.stage, "catalog-close-library") && strcmp(a.stage, "catalog-open-save") &&
        strcmp(a.stage, "catalog-save") &&
        strcmp(a.stage, "catalog-apply") && strcmp(a.stage, "catalog-undo") &&
        strcmp(a.stage, "catalog-delete") && strcmp(a.stage, "catalog-mutate") &&
        strcmp(a.stage, "catalog-rename-tab") &&
        strcmp(a.stage, "catalog-pane-rename-open") &&
        strcmp(a.stage, "catalog-pane-rename-action") &&
        strcmp(a.stage, "catalog-pane-rename-type") &&
        strcmp(a.stage, "catalog-pane-rename-save") &&
        strcmp(a.stage, "catalog-pane-palette-open") &&
        strcmp(a.stage, "catalog-pane-palette-query") &&
        strcmp(a.stage, "catalog-pane-palette-select") &&
        strcmp(a.stage, "catalog-stop-open") && strcmp(a.stage, "catalog-stop-action") &&
        strcmp(a.stage, "catalog-stop-cancel") && strcmp(a.stage, "catalog-stop-confirm") &&
        strcmp(a.stage, "catalog-sessions-open") && strcmp(a.stage, "catalog-sessions-action") &&
        strcmp(a.stage, "catalog-hidden-stop-review") && strcmp(a.stage, "catalog-hidden-stop-confirm") &&
        strcmp(a.stage, "catalog-profiles-open") && strcmp(a.stage, "catalog-profiles-query") &&
        strcmp(a.stage, "catalog-profiles-refresh") &&
        strcmp(a.stage, "catalog-profiles-select-first") && strcmp(a.stage, "catalog-profiles-select-next") &&
        strcmp(a.stage, "catalog-profiles-pin") && strcmp(a.stage, "catalog-profiles-move-up") &&
        strcmp(a.stage, "catalog-profiles-move-down") && strcmp(a.stage, "catalog-profiles-close") &&
        strcmp(a.stage, "catalog-git-open") && strcmp(a.stage, "catalog-git-query") &&
        strcmp(a.stage, "catalog-git-refresh") && strcmp(a.stage, "catalog-git-close") &&
        strcmp(a.stage, "catalog-git-diff-first") &&
        strcmp(a.stage, "catalog-git-diff-second") &&
        strcmp(a.stage, "catalog-git-index-apply") &&
        strcmp(a.stage, "catalog-git-commit-open") &&
        strcmp(a.stage, "catalog-git-commit-message") &&
        strcmp(a.stage, "catalog-git-commit-confirm") &&
        strcmp(a.stage, "catalog-close-open") && strcmp(a.stage, "catalog-close-query") &&
        strcmp(a.stage, "catalog-close-inspect") && strcmp(a.stage, "catalog-close-cancel") &&
        strcmp(a.stage, "catalog-close-confirm") &&
        strcmp(a.stage, "recipe-select-first") &&
        strcmp(a.stage, "recipe-open-editor") &&
        strcmp(a.stage, "recipe-save-editor") && strcmp(a.stage, "recipe-new-tab") &&
        strcmp(a.stage, "recipe-name-tab") && strcmp(a.stage, "recipe-submit-name") &&
        strcmp(a.stage, "recipe-confirm-run") && strcmp(a.stage, "recipe-slot-open") &&
        strcmp(a.stage, "recipe-slot-dirty") && strcmp(a.stage, "recipe-profile-open") &&
        strcmp(a.stage, "recipe-profile-no-match") && strcmp(a.stage, "recipe-profile-cancel") &&
        strcmp(a.stage, "recipe-profile-select") && strcmp(a.stage, "recipe-slot-save") &&
        strcmp(a.stage, "recipe-name-custom-tab") && strcmp(a.stage, "recipe-custom-confirm") &&
        strcmp(a.stage, "profile-open") &&
        strcmp(a.stage, "profile-new") && strcmp(a.stage, "profile-create-fields") &&
        strcmp(a.stage, "profile-context-off") &&
        strcmp(a.stage, "profile-save") && strcmp(a.stage, "profile-select-first") &&
        strcmp(a.stage, "profile-launch") && strcmp(a.stage, "profile-confirm") &&
        strcmp(a.stage, "profile-edit") && strcmp(a.stage, "profile-rename") &&
        strcmp(a.stage, "profile-save-edit") && strcmp(a.stage, "profile-delete") &&
        strcmp(a.stage, "profile-cancel-delete") && strcmp(a.stage, "profile-confirm-delete") &&
        strcmp(a.stage, "setup-open") &&
        strcmp(a.stage, "setup-create") && strcmp(a.stage, "setup-shell") &&
        strcmp(a.stage, "setup-launcher") && strcmp(a.stage, "setup-codex") &&
        strcmp(a.stage, "custom-open") && strcmp(a.stage, "custom-fields") &&
        strcmp(a.stage, "custom-save") && strcmp(a.stage, "custom-select") &&
        strcmp(a.stage, "custom-reopen-profiles") &&
        strcmp(a.stage, "custom-review-enter") && strcmp(a.stage, "custom-review-cancel") &&
        strcmp(a.stage, "custom-confirm") &&
        strcmp(a.stage, "setup-compact") && strcmp(a.stage, "setup-wide") &&
        strcmp(a.stage, "setup-split-right") && strcmp(a.stage, "setup-split-down") &&
        strcmp(a.stage, "setup-close") && strcmp(a.stage, "setup-focus-left") &&
        strcmp(a.stage, "setup-live") && strcmp(a.stage, "opencode-input") &&
        strcmp(a.stage, "controls-focus-toggle") && strcmp(a.stage, "controls-focus-right") &&
        strcmp(a.stage, "controls-alt-focus-toggle") &&
        strcmp(a.stage, "controls-swap") && strcmp(a.stage, "controls-resize-cancel") &&
        strcmp(a.stage, "controls-resize-commit") && strcmp(a.stage, "controls-close") &&
        strcmp(a.stage, "controls-open") && strcmp(a.stage, "controls-select-alt") &&
        strcmp(a.stage, "controls-select-control") && strcmp(a.stage, "controls-select-disabled") &&
        strcmp(a.stage, "appearance-open") && strcmp(a.stage, "appearance-light") &&
        strcmp(a.stage, "appearance-motion") && strcmp(a.stage, "appearance-dark") &&
        strcmp(a.stage, "appearance-terminal-dark") &&
        strcmp(a.stage, "appearance-terminal-light") &&
        strcmp(a.stage, "appearance-terminal-follow") &&
        strcmp(a.stage, "appearance-font-increase") && strcmp(a.stage, "appearance-font-decrease") &&
        strcmp(a.stage, "appearance-font-reset") &&
        strcmp(a.stage, "appearance-reload") && strcmp(a.stage, "appearance-close") &&
        strcmp(a.stage, "controls-move") && strcmp(a.stage, "pointer-hover-left") &&
        strcmp(a.stage, "pointer-hover-header") &&
        strcmp(a.stage, "pointer-hover-right") && strcmp(a.stage, "pointer-hover-input") &&
        strcmp(a.stage, "pointer-no-motion-release") &&
        strcmp(a.stage, "pointer-hover-source") &&
        strcmp(a.stage, "pointer-subthreshold-hold") &&
        strcmp(a.stage, "pointer-focus-toggle") &&
        strcmp(a.stage, "pointer-focus-mode-preview") &&
        strcmp(a.stage, "pointer-keyboard-away") && strcmp(a.stage, "pointer-move-right-near") &&
        strcmp(a.stage, "pointer-hold-preview") && strcmp(a.stage, "pointer-drop") &&
        strcmp(a.stage, "pointer-drop-gap") &&
        strcmp(a.stage, "pointer-create-transfer-tab") && strcmp(a.stage, "pointer-select-first-tab") &&
        strcmp(a.stage, "pointer-hotkey-transfer") && strcmp(a.stage, "pointer-picker-transfer") &&
        strcmp(a.stage, "pointer-picker-confirm") &&
        strcmp(a.stage, "pointer-tab-drop-cancel") && strcmp(a.stage, "pointer-tab-drop") &&
        strcmp(a.stage, "pointer-tab-drop-populated-preview") &&
        strcmp(a.stage, "pointer-file-drop") &&
        strcmp(a.stage, "pointer-file-drop-invalid-uri") &&
        strcmp(a.stage, "pointer-file-drop-truncated") &&
        strcmp(a.stage, "pointer-file-drop-cancel") &&
        strcmp(a.stage, "pointer-file-drop-confirm") &&
        strcmp(a.stage, "pointer-clipboard-paste") &&
        strcmp(a.stage, "pointer-clipboard-enter") &&
        strcmp(a.stage, "pointer-clipboard-cancel") &&
        strcmp(a.stage, "pointer-clipboard-confirm") &&
        strcmp(a.stage, "pointer-undo") && strcmp(a.stage, "pointer-selection-hold") &&
        strcmp(a.stage, "pointer-ime-input") && strcmp(a.stage, "pointer-ime-cancel") &&
        strcmp(a.stage, "pointer-dialog-hover") && strcmp(a.stage, "pointer-inactive-hold"))
        fail("unsupported stage");
    if (strcmp(a.stage, "preflight") && (!a.app || !a.app_start)) fail("app identity required");
    if (!strcmp(a.stage, "capture") && (!a.output || strncmp(a.output, "/artifacts/", 11)))
        fail("capture path must be under /artifacts");
    if (!strcmp(a.stage, "pointer-hold-preview") &&
        (!a.output || strcmp(a.output, "/artifacts/pointer-drag.ppm")))
        fail("pointer preview capture path must be the fixed artifact");
    if (!strcmp(a.stage, "pointer-hover-header") &&
        (!a.output || strcmp(a.output, "/artifacts/pointer-hover-header.ppm")))
        fail("pointer header hover capture path must be the fixed artifact");
    if (!strcmp(a.stage, "pointer-no-motion-release") &&
        (!a.output || strcmp(a.output, "/artifacts/pointer-no-motion-held.ppm")))
        fail("pointer no-motion capture path must be the fixed artifact");
    if (!strcmp(a.stage, "pointer-selection-hold") &&
        (!a.output || strcmp(a.output, "/artifacts/pointer-selection.ppm")))
        fail("pointer selection capture path must be the fixed artifact");
    if (!strcmp(a.stage, "pointer-subthreshold-hold") &&
        (!a.output || strcmp(a.output, "/artifacts/pointer-small-motion-held.ppm")))
        fail("small-motion pointer capture path must be the fixed artifact");
    if (!strcmp(a.stage, "pointer-focus-mode-preview") &&
        (!a.output || strcmp(a.output, "/artifacts/pointer-focus-mode-drag.ppm")))
        fail("focus-mode pointer capture path must be the fixed artifact");
    if (!strcmp(a.stage, "pointer-tab-drop-cancel") &&
        (!a.output || strcmp(a.output, "/artifacts/pointer-tab-drop-cancel.ppm")))
        fail("tab-drop cancel capture path must be the fixed artifact");
    if (!strcmp(a.stage, "pointer-tab-drop") &&
        (!a.output || strcmp(a.output, "/artifacts/pointer-tab-drop-preview.ppm")))
        fail("tab-drop preview capture path must be the fixed artifact");
    if (!strcmp(a.stage, "pointer-tab-drop-populated-preview") &&
        (!a.output || strcmp(a.output, "/artifacts/pointer-tab-drop-populated-preview.ppm")))
        fail("populated tab-drop preview capture path must be the fixed artifact");
    if (!strcmp(a.stage, "pointer-file-drop") &&
        (!a.output || (strcmp(a.output, "/artifacts/pointer-file-drop-review-cancel.ppm") &&
                      strcmp(a.output, "/artifacts/pointer-file-drop-review-confirm.ppm"))))
        fail("file-drop review capture path must be the fixed artifact");
    if (!strcmp(a.stage, "pointer-file-drop-invalid-uri") &&
        (!a.output || strcmp(a.output, "/artifacts/pointer-file-drop-invalid-uri.ppm")))
        fail("invalid URI-list capture path must be the fixed artifact");
    if (!strcmp(a.stage, "pointer-file-drop-truncated") &&
        (!a.output || strcmp(a.output, "/artifacts/pointer-file-drop-truncated.ppm")))
        fail("truncated URI-list capture path must be the fixed artifact");
    if (!strcmp(a.stage, "pointer-ime-input") &&
        (!a.output || strcmp(a.output, "/artifacts/pointer-ime-preedit.ppm")))
        fail("pointer IME preedit capture path must be the fixed artifact");
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

static void custom_ascii(Display *dpy, const char *text) {
    for (const unsigned char *p = (const unsigned char *)text; *p; ++p) {
        KeySym sym = NoSymbol;
        int shifted = 0;
        if (*p >= 'a' && *p <= 'z') sym = (KeySym)*p;
        else if (*p >= 'A' && *p <= 'Z') { sym = (KeySym)(*p - 'A' + 'a'); shifted = 1; }
        else if (*p >= '0' && *p <= '9') sym = (KeySym)*p;
        else switch (*p) {
            case ' ': sym = XK_space; break;
            case '-': sym = XK_minus; break;
            case '_': sym = XK_minus; shifted = 1; break;
            case '/': sym = XK_slash; break;
            case '.': sym = XK_period; break;
            case '"': sym = XK_apostrophe; shifted = 1; break;
            case '\'': sym = XK_apostrophe; break;
            case '$': sym = XK_4; shifted = 1; break;
            case '(': sym = XK_9; shifted = 1; break;
            case ')': sym = XK_0; shifted = 1; break;
            case ';': sym = XK_semicolon; break;
            default: fail("fixed custom-profile ASCII contains an unsupported character");
        }
        if (shifted) modified_key(dpy, XK_Shift_L, sym);
        else key(dpy, sym);
    }
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
    modified_key(dpy, XK_Shift_L, XK_Return);
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

static void setup_prefix(Display *dpy, KeySym final_key, int shift_final) {
    KeyCode control=XKeysymToKeycode(dpy,XK_Control_L), shift=XKeysymToKeycode(dpy,XK_Shift_L);
    if (!control || !shift || !XTestFakeKeyEvent(dpy,control,True,CurrentTime) ||
        !XTestFakeKeyEvent(dpy,shift,True,CurrentTime)) fail("setup prefix press failed");
    key(dpy,XK_space);
    if (!XTestFakeKeyEvent(dpy,control,False,CurrentTime) ||
        !XTestFakeKeyEvent(dpy,shift,False,CurrentTime)) fail("setup prefix release failed");
    if (shift_final) modified_key(dpy,XK_Shift_L,final_key); else key(dpy,final_key);
}

static void control_shift_key(Display *dpy, KeySym sym) {
    KeyCode control = XKeysymToKeycode(dpy, XK_Control_L);
    KeyCode shift = XKeysymToKeycode(dpy, XK_Shift_L);
    if (!control || !shift || !XTestFakeKeyEvent(dpy, control, True, CurrentTime) ||
        !XTestFakeKeyEvent(dpy, shift, True, CurrentTime))
        fail("fixed Ctrl+Shift key press failed");
    key(dpy, sym);
    if (!XTestFakeKeyEvent(dpy, shift, False, CurrentTime) ||
        !XTestFakeKeyEvent(dpy, control, False, CurrentTime))
        fail("fixed Ctrl+Shift key release failed");
    XSync(dpy, False);
}

static void catalog_rename_tab(Display *dpy, Window app) {
    XWindowAttributes attr;
    if (!XGetWindowAttributes(dpy, app, &attr) || attr.width != 1280 || attr.height != 800)
        fail("catalog rename requires the fixed private 1280x800 app");
    int root_x = 0, root_y = 0;
    Window child = None;
    if (!XTranslateCoordinates(dpy, app, DefaultRootWindow(dpy), 280, 58,
                                &root_x, &root_y, &child) ||
        !XTestFakeMotionEvent(dpy, DefaultScreen(dpy), root_x, root_y, CurrentTime) ||
        !XTestFakeButtonEvent(dpy, 3, True, CurrentTime) ||
        !XTestFakeButtonEvent(dpy, 3, False, CurrentTime))
        fail("fixed selected-tab context click failed");
    XSync(dpy, False);
    struct timespec menu_settle = { .tv_sec = 0, .tv_nsec = 180000000 };
    nanosleep(&menu_settle, NULL);
    key(dpy, XK_Down); key(dpy, XK_Return);
    modified_key(dpy, XK_Control_L, XK_a);
    modified_key(dpy, XK_Shift_L, XK_r);
    const KeySym name[] = { XK_e, XK_n, XK_a, XK_m, XK_e, XK_d };
    for (size_t i = 0; i < sizeof(name) / sizeof(name[0]); ++i) key(dpy, name[i]);
    key(dpy, XK_Return);
}

/* Fixed product coordinates only, after private server/window verification.
 * No arbitrary pointer coordinates are accepted from arguments. */
static void setup_click(Display *dpy, Window app, int x, int y) {
    XWindowAttributes attr;
    if (!XGetWindowAttributes(dpy, app, &attr) || attr.width != 1280 || attr.height != 800)
        fail("setup click requires the fixed private 1280x800 app");
    Window child; int root_x, root_y;
    if (!XTranslateCoordinates(dpy, app, DefaultRootWindow(dpy), x, y,
                               &root_x, &root_y, &child)) fail("setup coordinate translation failed");
    if (!XTestFakeMotionEvent(dpy, DefaultScreen(dpy), root_x, root_y, CurrentTime) ||
        !XTestFakeButtonEvent(dpy, 1, True, CurrentTime) ||
        !XTestFakeButtonEvent(dpy, 1, False, CurrentTime)) fail("private setup click failed");
    XSync(dpy, False);
}

/* Fixed motion grid plus one fixed unsent string for the isolated OpenCode PTY. */
static void opencode_input_sequence(Display *dpy, Window app) {
    XWindowAttributes attr;
    if (!XGetWindowAttributes(dpy, app, &attr) || attr.width != 1280 || attr.height != 800)
        fail("OpenCode input requires the fixed private 1280x800 app");
    for (int row = 0; row < 8; ++row) {
        for (int column = 0; column < 16; ++column) {
            int x = 650 + column * 39;
            int y = 80 + row * 92;
            Window child; int root_x, root_y;
            if (!XTranslateCoordinates(dpy, app, DefaultRootWindow(dpy), x, y,
                                       &root_x, &root_y, &child) ||
                !XTestFakeMotionEvent(dpy, DefaultScreen(dpy), root_x, root_y, CurrentTime))
                fail("fixed OpenCode terminal motion failed");
            XFlush(dpy);
        }
    }
    XSync(dpy, False);
    struct timespec settle = { .tv_sec = 0, .tv_nsec = 250000000 };
    nanosleep(&settle, NULL);
    const KeySym prompt[] = {
        XK_p, XK_i, XK_g, XK_a, XK_d, XK_e, XK_i, XK_n,
        XK_p, XK_u, XK_t, XK_w, XK_o, XK_r, XK_k, XK_s
    };
    for (size_t i = 0; i < sizeof(prompt) / sizeof(prompt[0]); ++i)
        key(dpy, prompt[i]);
}

static void controls_drag(Display *dpy, Window app, int button, int escape) {
    XWindowAttributes attr;
    if (!XGetWindowAttributes(dpy, app, &attr) || attr.width != 1280 || attr.height != 800)
        fail("controls drag requires the fixed private 1280x800 app");
    int start_x = button == 1 ? 485 : 720;
    int start_y = 250;
    int end_x = button == 1 ? 1015 : 660;
    int end_y = 250;
    Window child; int root_x, root_y;
    KeyCode control = XKeysymToKeycode(dpy, XK_Control_L);
    if (!control || !XTranslateCoordinates(dpy, app, DefaultRootWindow(dpy), start_x, start_y,
                                            &root_x, &root_y, &child) ||
        !XTestFakeKeyEvent(dpy, control, True, CurrentTime) ||
        !XTestFakeMotionEvent(dpy, DefaultScreen(dpy), root_x, root_y, CurrentTime))
        fail("controls drag start failed");
    XSync(dpy, False);
    if (!XTestFakeButtonEvent(dpy, (unsigned int)button, True, CurrentTime) ||
        !XTranslateCoordinates(dpy, app, DefaultRootWindow(dpy), end_x, end_y,
                               &root_x, &root_y, &child) ||
        !XTestFakeMotionEvent(dpy, DefaultScreen(dpy), root_x, root_y, CurrentTime))
        fail("controls drag motion failed");
    XSync(dpy, False);
    if (escape) key(dpy, XK_Escape);
    if (!XTestFakeButtonEvent(dpy, (unsigned int)button, False, CurrentTime) ||
        !XTestFakeKeyEvent(dpy, control, False, CurrentTime))
        fail("controls drag release failed");
    XSync(dpy, False);
}

static void pointer_position(Display *dpy, Window app, int x, int y) {
    Window child; int root_x, root_y;
    if (!XTranslateCoordinates(dpy, app, DefaultRootWindow(dpy), x, y,
                               &root_x, &root_y, &child) ||
        !XTestFakeMotionEvent(dpy, DefaultScreen(dpy), root_x, root_y, CurrentTime))
        fail("fixed pointer motion failed");
    XFlush(dpy);
}

static void scrollback_wheel(Display *dpy, Window app) {
    XWindowAttributes attr;
    if (!XGetWindowAttributes(dpy, app, &attr) || attr.width != 1280 || attr.height != 800)
        fail("scrollback wheel requires the fixed private 1280x800 app");
    pointer_position(dpy, app, 640, 400);
    for (int i = 0; i < 4; ++i) {
        if (!XTestFakeButtonEvent(dpy, 4, True, CurrentTime) ||
            !XTestFakeButtonEvent(dpy, 4, False, CurrentTime))
            fail("fixed unmodified scrollback wheel event failed");
    }
    XSync(dpy, False);
}

static void pointer_hover(Display *dpy, Window app, int x, int y, int burst) {
    XWindowAttributes attr;
    if (!XGetWindowAttributes(dpy, app, &attr) || attr.width != 1280 || attr.height != 800)
        fail("pointer hover requires the fixed private 1280x800 app");
    pointer_position(dpy, app, x, y);
    if (burst) {
        struct timespec pause = { .tv_sec = 0, .tv_nsec = 50000000 };
        nanosleep(&pause, NULL);
        pointer_position(dpy, app, 1015, 250);
        nanosleep(&pause, NULL);
        pointer_position(dpy, app, 485, 250);
    }
    XSync(dpy, False);
    if (burst) {
        struct timespec settle = { .tv_sec = 0, .tv_nsec = 150000000 };
        nanosleep(&settle, NULL);
        const KeySym prompt[] = { XK_p, XK_i, XK_g, XK_a, XK_d, XK_e,
                                  XK_h, XK_o, XK_v, XK_e, XK_r };
        for (size_t i = 0; i < sizeof(prompt) / sizeof(prompt[0]); ++i)
            key(dpy, prompt[i]);
    }
}

static void pointer_drag_begin(Display *dpy, Window app) {
    XWindowAttributes attr;
    if (!XGetWindowAttributes(dpy, app, &attr) || attr.width != 1280 || attr.height != 800)
        fail("pointer drag requires the fixed private 1280x800 app");
    KeyCode control = XKeysymToKeycode(dpy, XK_Control_L);
    if (!control || !XTestFakeKeyEvent(dpy, control, True, CurrentTime))
        fail("pointer drag modifier press failed");
    /* Start left of center so the held capture exposes the original grip offset. */
    pointer_position(dpy, app, 350, 250);
    if (!XTestFakeButtonEvent(dpy, 1, True, CurrentTime))
        fail("pointer drag button press failed");
    /* The detached target spans x755..1280; stay clearly in its right half. */
    pointer_position(dpy, app, 1100, 250);
    XSync(dpy, False);
}

static void pointer_gap_drag(Display *dpy, Window app) {
    XWindowAttributes attr;
    if (!XGetWindowAttributes(dpy, app, &attr) || attr.width != 1280 || attr.height != 800)
        fail("pointer gap drop requires the fixed private 1280x800 app");
    KeyCode control = XKeysymToKeycode(dpy, XK_Control_L);
    if (!control || !XTestFakeKeyEvent(dpy, control, True, CurrentTime))
        fail("pointer gap drop modifier press failed");
    pointer_position(dpy, app, 350, 250);
    if (!XTestFakeButtonEvent(dpy, 1, True, CurrentTime))
        fail("pointer gap drop button press failed");
    /* x=754 is the fixed detached gutter immediately before the right target at x=755. */
    pointer_position(dpy, app, 754, 250);
    XSync(dpy, False);
}

static void pointer_tab_drag_begin(Display *dpy, Window app) {
    XWindowAttributes attr;
    if (!XGetWindowAttributes(dpy, app, &attr) || attr.width != 1280 || attr.height != 800)
        fail("tab drop requires the fixed private 1280x800 app");
    KeyCode control = XKeysymToKeycode(dpy, XK_Control_L);
    if (!control || !XTestFakeKeyEvent(dpy, control, True, CurrentTime))
        fail("tab drop modifier press failed");
    pointer_position(dpy, app, 350, 250);
    if (!XTestFakeButtonEvent(dpy, 1, True, CurrentTime))
        fail("tab drop button press failed");
    /* Fixed center of the second visible tab in the private four-shell fixture. */
    pointer_position(dpy, app, 405, 57);
    XSync(dpy, False);
}

static void pointer_no_motion_begin(Display *dpy, Window app) {
    XWindowAttributes attr;
    if (!XGetWindowAttributes(dpy, app, &attr) || attr.width != 1280 || attr.height != 800)
        fail("pointer no-motion probe requires the fixed private 1280x800 app");
    KeyCode control = XKeysymToKeycode(dpy, XK_Control_L);
    if (!control || !XTestFakeKeyEvent(dpy, control, True, CurrentTime))
        fail("pointer no-motion modifier press failed");
    pointer_position(dpy, app, 350, 250);
    if (!XTestFakeButtonEvent(dpy, 1, True, CurrentTime))
        fail("pointer no-motion button press failed");
    XSync(dpy, False);
}

static void pointer_drag_release(Display *dpy, int cancel) {
    KeyCode control = XKeysymToKeycode(dpy, XK_Control_L);
    if (cancel) key(dpy, XK_Escape);
    if (!XTestFakeButtonEvent(dpy, 1, False, CurrentTime) || !control ||
        !XTestFakeKeyEvent(dpy, control, False, CurrentTime))
        fail("pointer drag release failed");
    XSync(dpy, False);
}

static void pointer_selection_hold(Display *dpy, Window app, const char *output) {
    XWindowAttributes attr;
    if (!XGetWindowAttributes(dpy, app, &attr) || attr.width != 1280 || attr.height != 800)
        fail("pointer selection requires the fixed private 1280x800 app");
    pointer_position(dpy, app, 485, 250);
    if (!XTestFakeButtonEvent(dpy, 1, True, CurrentTime)) fail("selection button press failed");
    pointer_position(dpy, app, 1015, 250);
    XSync(dpy, False);
    struct timespec settle = { .tv_sec = 0, .tv_nsec = 200000000 };
    nanosleep(&settle, NULL);
    capture(dpy, output);
    puts("SELECTING"); fflush(stdout);
    char response[32];
    if (!fgets(response, sizeof(response), stdin) || strcmp(response, "RELEASE\n"))
        fail("pointer selection accepts only fixed release");
    if (!XTestFakeButtonEvent(dpy, 1, False, CurrentTime)) fail("selection button release failed");
    XSync(dpy, False);
}

static void pointer_inactive_hold(const Args *a, Display *dpy, xcb_connection_t *c) {
    Window app = verify_app_focus_target(a, dpy, c, 0);
    XSetInputFocus(dpy, DefaultRootWindow(dpy), RevertToParent, CurrentTime);
    XSync(dpy, False);
    /* XSync drains the server, not GPUI's activation callback. */
    struct timespec settle = { .tv_sec = 0, .tv_nsec = 200000000 };
    nanosleep(&settle, NULL);
    pointer_position(dpy, app, 1015, 250);
    XSync(dpy, False);
    puts("INACTIVE"); fflush(stdout);
    char response[32];
    if (!fgets(response, sizeof(response), stdin) || strcmp(response, "RESTORE\n"))
        fail("inactive pointer stage accepts only fixed focus restoration");
    /* Restore focus without a second motion: deferred hover must be applied. */
    XSetInputFocus(dpy, app, RevertToParent, CurrentTime);
    XSync(dpy, False);
}

/* Send exactly one native XDND URI-list to the verified fixture app window.
 * This source and its selection owner exist only on the private Xvfb. */
enum FixedUriPayload { URI_PAYLOAD_VALID, URI_PAYLOAD_NONFILE, URI_PAYLOAD_TRUNCATED };

static const unsigned char *fixed_uri_payload(enum FixedUriPayload kind, size_t *length) {
    static const unsigned char valid[] =
        "file:///home/pigade/demo/a%20space.txt\r\n"
        "file:///home/pigade/demo/it%27s%24%28no%29.txt\r\n";
    static const unsigned char nonfile[] =
        "file:///home/pigade/demo/a%20space.txt\r\n"
        "https://example.invalid/file.txt\r\n";
    static unsigned char truncated[66 * 1024];
    static size_t truncated_length;
    if (kind == URI_PAYLOAD_VALID) {
        *length = sizeof(valid) - 1;
        return valid;
    }
    if (kind == URI_PAYLOAD_NONFILE) {
        *length = sizeof(nonfile) - 1;
        return nonfile;
    }
    if (!truncated_length) {
        static const char prefix[] = "file:///home/pigade/demo/a%20space.txt\r\n#";
        memcpy(truncated, prefix, sizeof(prefix) - 1);
        memset(truncated + sizeof(prefix) - 1, 'x', sizeof(truncated) - sizeof(prefix) - 1);
        truncated[sizeof(truncated) - 2] = '\r';
        truncated[sizeof(truncated) - 1] = '\n';
        truncated_length = sizeof(truncated);
    }
    *length = truncated_length;
    return truncated;
}

static int serve_xdnd_request(Display *dpy, xcb_connection_t *c, Window source,
                              pid_t app_pid, Atom selection, Atom uri_list,
                              enum FixedUriPayload payload, XSelectionRequestEvent *request) {
    if (request->owner != source || request->selection != selection ||
        xres_pid(c, (uint32_t)request->requestor) != app_pid ||
        (request->target != uri_list && request->target != XA_ATOM))
        fail("unexpected XDND selection request");
    Atom output_property = request->property == None ? request->target : request->property;
    if (request->target == XA_ATOM) {
        Atom offered[] = { uri_list, XA_ATOM };
        XChangeProperty(dpy, request->requestor, output_property, XA_ATOM, 32,
                        PropModeReplace, (unsigned char *)offered, 2);
    } else {
        size_t length = 0;
        const unsigned char *uris = fixed_uri_payload(payload, &length);
        XChangeProperty(dpy, request->requestor, output_property, uri_list, 8,
                        PropModeReplace, uris, (int)length);
    }
    XEvent reply; memset(&reply, 0, sizeof(reply));
    reply.xselection.type = SelectionNotify;
    reply.xselection.display = dpy;
    reply.xselection.requestor = request->requestor;
    reply.xselection.selection = request->selection;
    reply.xselection.target = request->target;
    reply.xselection.property = output_property;
    reply.xselection.time = request->time;
    if (!XSendEvent(dpy, request->requestor, False, 0, &reply))
        fail("XDND SelectionNotify send failed");
    XFlush(dpy);
    return request->target == uri_list;
}

static void pointer_file_drop(Display *dpy, xcb_connection_t *c, Window target, pid_t app_pid,
                              enum FixedUriPayload payload) {
    Atom aware = XInternAtom(dpy, "XdndAware", False);
    Atom enter = XInternAtom(dpy, "XdndEnter", False);
    Atom position = XInternAtom(dpy, "XdndPosition", False);
    Atom drop = XInternAtom(dpy, "XdndDrop", False);
    Atom status = XInternAtom(dpy, "XdndStatus", False);
    Atom finished = XInternAtom(dpy, "XdndFinished", False);
    Atom leave = XInternAtom(dpy, "XdndLeave", False);
    Atom selection = XInternAtom(dpy, "XdndSelection", False);
    Atom uri_list = XInternAtom(dpy, "text/uri-list", False);
    Atom actual_type = None; int actual_format = 0; unsigned long nitems = 0, after = 0;
    unsigned char *value = NULL;
    int rc = XGetWindowProperty(dpy, target, aware, 0, 1, False, XA_ATOM,
                                &actual_type, &actual_format, &nitems, &after, &value);
    int version_ok = rc == Success && actual_type == XA_ATOM && actual_format == 32 &&
                     nitems == 1 && value && (*(unsigned long *)value) >= 3;
    if (value) XFree(value);
    if (!version_ok) fail("verified app window does not advertise XDND version 3 or newer");
    Window source = XCreateSimpleWindow(dpy, DefaultRootWindow(dpy), 0, 0, 1, 1, 0, 0, 0);
    if (!source) fail("cannot create private XDND source window");
    XSelectInput(dpy, source, StructureNotifyMask | PropertyChangeMask);
    if (!XSetSelectionOwner(dpy, selection, source, CurrentTime))
        fail("cannot own private XDND selection");
    XSync(dpy, False);
    if (XGetSelectionOwner(dpy, selection) != source)
        fail("private XDND selection ownership was not established");
    XWindowAttributes attr; int root_x = 0, root_y = 0; Window child = None;
    if (!XGetWindowAttributes(dpy, target, &attr) || attr.width != 1280 || attr.height != 800 ||
        !XTranslateCoordinates(dpy, target, DefaultRootWindow(dpy), 350, 250,
                               &root_x, &root_y, &child))
        fail("file drop requires the fixed private 1280x800 app target");
    XEvent event; memset(&event, 0, sizeof(event));
    event.xclient.type = ClientMessage; event.xclient.display = dpy;
    event.xclient.window = target; event.xclient.message_type = enter; event.xclient.format = 32;
    event.xclient.data.l[0] = (long)source;
    event.xclient.data.l[1] = 5L << 24;
    event.xclient.data.l[2] = (long)uri_list;
    if (!XSendEvent(dpy, target, False, NoEventMask, &event)) fail("XDND Enter send failed");
    XFlush(dpy);
    memset(&event, 0, sizeof(event));
    event.xclient.type = ClientMessage; event.xclient.display = dpy;
    event.xclient.window = target; event.xclient.message_type = position; event.xclient.format = 32;
    event.xclient.data.l[0] = (long)source;
    event.xclient.data.l[2] = ((root_x & 0xffff) << 16) | (root_y & 0xffff);
    event.xclient.data.l[3] = CurrentTime;
    event.xclient.data.l[4] = (long)XInternAtom(dpy, "XdndActionCopy", False);
    if (!XSendEvent(dpy, target, False, NoEventMask, &event)) fail("XDND Position send failed");
    XFlush(dpy);
    int status_received = 0, accepted = 0, uri_transferred = 0;
    struct timespec deadline; clock_gettime(CLOCK_MONOTONIC, &deadline); deadline.tv_sec += 2;
    while (!status_received || !uri_transferred) {
        while (XPending(dpy)) {
            XNextEvent(dpy, &event);
            if (event.type == SelectionRequest) {
                uri_transferred |= serve_xdnd_request(dpy, c, source, app_pid,
                    selection, uri_list, payload, &event.xselectionrequest);
            } else if (event.type == ClientMessage && event.xclient.message_type == status) {
                if ((Window)event.xclient.data.l[0] != target)
                    fail("XDND Status came from an unexpected target");
                status_received = 1;
                accepted = (event.xclient.data.l[1] & 1L) != 0 &&
                    event.xclient.data.l[4] == (long)XInternAtom(dpy, "XdndActionCopy", False);
            }
        }
        if (status_received && uri_transferred) break;
        struct timespec now; clock_gettime(CLOCK_MONOTONIC, &now);
        long ms = (deadline.tv_sec - now.tv_sec) * 1000L +
                  (deadline.tv_nsec - now.tv_nsec + 999999L) / 1000000L;
        if (ms <= 0) fail("XDND Status deadline expired");
        struct pollfd pfd = { .fd = ConnectionNumber(dpy), .events = POLLIN };
        if (poll(&pfd, 1, (int)ms) < 0 && errno != EINTR) fail("XDND Status wait failed");
    }
    /* GPUI converts the URI list while Position is active. Let that target-side
     * event task settle before sending Drop; this is fixture pacing, not a
     * performance measurement. */
    struct timespec entered = { .tv_sec = 0, .tv_nsec = 250000000 };
    nanosleep(&entered, NULL);
    if (payload != URI_PAYLOAD_VALID && !accepted) {
        memset(&event, 0, sizeof(event));
        event.xclient.type = ClientMessage; event.xclient.display = dpy;
        event.xclient.window = target; event.xclient.message_type = leave; event.xclient.format = 32;
        event.xclient.data.l[0] = (long)source;
        if (!XSendEvent(dpy, target, False, NoEventMask, &event)) fail("XDND Leave send failed");
        XFlush(dpy);
        struct timespec settle = { .tv_sec = 0, .tv_nsec = 250000000 };
        nanosleep(&settle, NULL);
        XSetSelectionOwner(dpy, selection, None, CurrentTime);
        XDestroyWindow(dpy, source);
        XSync(dpy, False);
        return;
    }
    if (!accepted) fail("verified app rejected valid fixed URI-list drop");
    memset(&event, 0, sizeof(event));
    event.xclient.type = ClientMessage; event.xclient.display = dpy;
    event.xclient.window = target; event.xclient.message_type = drop; event.xclient.format = 32;
    event.xclient.data.l[0] = (long)source; event.xclient.data.l[2] = CurrentTime;
    if (!XSendEvent(dpy, target, False, NoEventMask, &event)) fail("XDND Drop send failed");
    XFlush(dpy);
    int complete = 0; clock_gettime(CLOCK_MONOTONIC, &deadline); deadline.tv_sec += 2;
    while (!complete) {
        while (XPending(dpy)) {
            XNextEvent(dpy, &event);
            if (event.type == SelectionRequest) {
                uri_transferred |= serve_xdnd_request(dpy, c, source, app_pid,
                    selection, uri_list, payload, &event.xselectionrequest);
            } else if (event.type == ClientMessage && event.xclient.message_type == finished) {
                if ((Window)event.xclient.data.l[0] != target)
                    fail("XDND Finished came from an unexpected target");
                int succeeded = (event.xclient.data.l[1] & 1L) != 0;
                if ((payload == URI_PAYLOAD_VALID) != succeeded)
                    fail("XDND Finished did not match fixed URI-list validity");
                complete = 1;
            }
        }
        if (complete) break;
        struct timespec now; clock_gettime(CLOCK_MONOTONIC, &now);
        long ms = (deadline.tv_sec - now.tv_sec) * 1000L +
                  (deadline.tv_nsec - now.tv_nsec + 999999L) / 1000000L;
        if (ms <= 0) fail("XDND Finished deadline expired");
        struct pollfd pfd = { .fd = ConnectionNumber(dpy), .events = POLLIN };
        if (poll(&pfd, 1, (int)ms) < 0 && errno != EINTR) fail("XDND Finished wait failed");
    }
    struct timespec review_paint = { .tv_sec = 0, .tv_nsec = 250000000 };
    nanosleep(&review_paint, NULL);
    XSetSelectionOwner(dpy, selection, None, CurrentTime);
    XDestroyWindow(dpy, source);
    XSync(dpy, False);
}

static int self_test_uri_payloads(void) {
    size_t invalid_length = 0, truncated_length = 0;
    const unsigned char *invalid = fixed_uri_payload(URI_PAYLOAD_NONFILE, &invalid_length);
    const unsigned char *truncated = fixed_uri_payload(URI_PAYLOAD_TRUNCATED, &truncated_length);
    if (!strstr((const char *)invalid, "https://example.invalid/file.txt") ||
        invalid_length <= strlen("file:///home/pigade/demo/a%20space.txt\r\n") ||
        truncated_length <= 65536 || truncated[truncated_length - 2] != '\r' ||
        truncated[truncated_length - 1] != '\n') return 1;
    printf("{\"invalid_uri_bytes\":%zu,\"truncated_uri_bytes\":%zu}\n",
           invalid_length, truncated_length);
    return 0;
}

/* Own one fixed multiline clipboard only during the private pointer fixture. */
static void pointer_clipboard_paste(Display *dpy, xcb_connection_t *c, pid_t app_pid,
                                   pid_t fcitx_pid) {
    Atom clipboard = XInternAtom(dpy, "CLIPBOARD", False);
    Atom targets = XInternAtom(dpy, "TARGETS", False);
    Atom utf8 = XInternAtom(dpy, "UTF8_STRING", False);
    Atom plain = XInternAtom(dpy, "text/plain", False);
    Atom plain_utf8 = XInternAtom(dpy, "text/plain;charset=utf-8", False);
    Window source = XCreateSimpleWindow(dpy, DefaultRootWindow(dpy), 0, 0, 1, 1, 0, 0, 0);
    if (!source) fail("cannot create private clipboard owner window");
    XSelectInput(dpy, source, StructureNotifyMask | PropertyChangeMask);
    XSetSelectionOwner(dpy, clipboard, source, CurrentTime);
    XSync(dpy, False);
    if (XGetSelectionOwner(dpy, clipboard) != source)
        fail("private clipboard ownership was not established");
    KeyCode control = XKeysymToKeycode(dpy, XK_Control_L);
    KeyCode shift = XKeysymToKeycode(dpy, XK_Shift_L);
    if (!control || !shift || !XTestFakeKeyEvent(dpy, control, True, CurrentTime) ||
        !XTestFakeKeyEvent(dpy, shift, True, CurrentTime))
        fail("fixed clipboard paste shortcut press failed");
    key(dpy, XK_v);
    if (!XTestFakeKeyEvent(dpy, shift, False, CurrentTime) ||
        !XTestFakeKeyEvent(dpy, control, False, CurrentTime))
        fail("fixed clipboard paste shortcut release failed");
    XFlush(dpy);
    int transferred = 0;
    struct timespec deadline; clock_gettime(CLOCK_MONOTONIC, &deadline); deadline.tv_sec += 2;
    while (!transferred) {
        XEvent event;
        while (XPending(dpy)) {
            XNextEvent(dpy, &event);
            if (event.type != SelectionRequest) continue;
            XSelectionRequestEvent *request = &event.xselectionrequest;
            int owner_match = request->owner == source;
            int selection_match = request->selection == clipboard;
            pid_t requestor_pid = xres_pid(c, (uint32_t)request->requestor);
            int target_match = request->target == targets || request->target == utf8 ||
                               request->target == plain || request->target == plain_utf8;
            char target_name[65] = "<unknown>";
            char *atom_name = XGetAtomName(dpy, request->target);
            if (atom_name) {
                snprintf(target_name, sizeof(target_name), "%.*s", 64, atom_name);
                XFree(atom_name);
            }
            if (!owner_match || !selection_match) {
                fprintf(stderr,
                    "clipboard_request_diagnostic owner_match=%d selection_match=%d "
                    "requestor_pid=%ld expected_pid=%ld target=%s target_allowed=%d\n",
                    owner_match, selection_match, (long)requestor_pid, (long)app_pid,
                    target_name, target_match);
                fflush(stderr);
                fail("unexpected private clipboard selection request");
            }
            if (fcitx_pid > 0 && fcitx_pid != app_pid && requestor_pid == fcitx_pid) {
                /* The already-verified private Fcitx client probes clipboard
                 * owners. Refuse its request with no data, then keep serving
                 * the exact app PID request below. */
                XEvent refusal; memset(&refusal, 0, sizeof(refusal));
                refusal.xselection.type = SelectionNotify;
                refusal.xselection.display = dpy;
                refusal.xselection.requestor = request->requestor;
                refusal.xselection.selection = request->selection;
                refusal.xselection.target = request->target;
                refusal.xselection.property = None;
                refusal.xselection.time = request->time;
                if (!XSendEvent(dpy, request->requestor, False, 0, &refusal))
                    fail("private Fcitx clipboard refusal failed");
                XFlush(dpy);
                continue;
            }
            if (requestor_pid != app_pid || !target_match) {
                fprintf(stderr,
                    "clipboard_request_diagnostic owner_match=%d selection_match=%d "
                    "requestor_pid=%ld expected_pid=%ld target=%s target_allowed=%d\n",
                    owner_match, selection_match, (long)requestor_pid, (long)app_pid,
                    target_name, target_match);
                fflush(stderr);
                fail("unexpected private clipboard selection request");
            }
            Atom output_property = request->property == None ? request->target : request->property;
            if (request->target == targets) {
                Atom offered[] = { targets, utf8, plain, plain_utf8 };
                XChangeProperty(dpy, request->requestor, output_property, XA_ATOM, 32,
                                PropModeReplace, (unsigned char *)offered, 4);
            } else {
                XChangeProperty(dpy, request->requestor, output_property, request->target, 8,
                                PropModeReplace, FIXED_CLIPBOARD_TEXT,
                                (int)(sizeof(FIXED_CLIPBOARD_TEXT) - 1));
                transferred = 1;
            }
            XEvent reply; memset(&reply, 0, sizeof(reply));
            reply.xselection.type = SelectionNotify;
            reply.xselection.display = dpy;
            reply.xselection.requestor = request->requestor;
            reply.xselection.selection = request->selection;
            reply.xselection.target = request->target;
            reply.xselection.property = output_property;
            reply.xselection.time = request->time;
            if (!XSendEvent(dpy, request->requestor, False, 0, &reply))
                fail("clipboard SelectionNotify send failed");
            XFlush(dpy);
        }
        if (transferred) break;
        struct timespec now; clock_gettime(CLOCK_MONOTONIC, &now);
        long ms = (deadline.tv_sec - now.tv_sec) * 1000L +
                  (deadline.tv_nsec - now.tv_nsec + 999999L) / 1000000L;
        if (ms <= 0) fail("clipboard transfer deadline expired");
        struct pollfd pfd = { .fd = ConnectionNumber(dpy), .events = POLLIN };
        if (poll(&pfd, 1, (int)ms) < 0 && errno != EINTR)
            fail("clipboard selection wait failed");
    }
    struct timespec settle = { .tv_sec = 0, .tv_nsec = 100000000 };
    nanosleep(&settle, NULL);
    XSetSelectionOwner(dpy, clipboard, None, CurrentTime);
    XDestroyWindow(dpy, source);
    XSync(dpy, False);
}

static void setup_create_sequence(Display *dpy) {
    const KeySym name[] = { XK_d, XK_e, XK_m, XK_o };
    for (size_t i = 0; i < sizeof(name) / sizeof(name[0]); ++i) key(dpy, name[i]);
    modified_key(dpy, XK_Control_L, XK_Return);
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

/* Wait for the fixed primary action to be painted, rather than clicking a
 * coordinate while the previous project-creation modal is still on screen. */
static void wait_workspace_bar(Display *dpy, Window app) {
    Window child; int x,y;
    if (!XTranslateCoordinates(dpy,app,DefaultRootWindow(dpy),1078,55,&x,&y,&child))
        fail("workspace bar coordinate translation failed");
    uint64_t deadline=monotonic_ns()+UINT64_C(5000000000);
    while (monotonic_ns()<deadline) {
        XImage *image=XGetImage(dpy,DefaultRootWindow(dpy),x,y,1,1,AllPlanes,ZPixmap);
        if (!image) fail("workspace bar pixel readback failed");
        unsigned long pixel=XGetPixel(image,0,0);
        int ready=component(pixel,image->red_mask)==184 && component(pixel,image->green_mask)==166 && component(pixel,image->blue_mask)==239;
        XDestroyImage(image);
        if (ready) return;
        struct timespec pause={.tv_sec=0,.tv_nsec=20000000}; nanosleep(&pause,NULL);
    }
    fail("workspace toolbar did not render before fixed click");
}

int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "--self-test-clipboard-payload")) {
        static const unsigned char expected[] =
            "PIGADE_MULTILINE_A\nPIGADE_MULTILINE_B";
        if (sizeof(expected) != sizeof(FIXED_CLIPBOARD_TEXT) ||
            memcmp(expected, FIXED_CLIPBOARD_TEXT, sizeof(expected)) != 0) return 3;
        for (size_t i = 0; i < sizeof(FIXED_CLIPBOARD_TEXT) - 1; ++i)
            printf("%02x", FIXED_CLIPBOARD_TEXT[i]);
        putchar('\n');
        return 0;
    }
    if (argc == 2 && !strcmp(argv[1], "--self-test-uri-payloads"))
        return self_test_uri_payloads();
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
    } else if (!strcmp(a.stage, "pointer-inactive-hold")) {
        pointer_inactive_hold(&a, dpy, c);
        puts("{\"stage\":\"pointer-inactive-hold\",\"focus_restored\":true}");
    } else {
        Window app_window = verify_app_focus_target(&a, dpy, c, 1);
        if (!strcmp(a.stage, "compose")) {
            const KeySym sequence[] = { XK_n, XK_i, XK_h, XK_a, XK_o };
            for (size_t i = 0; i < sizeof(sequence) / sizeof(sequence[0]); ++i) key(dpy, sequence[i]);
        } else if (!strcmp(a.stage, "context-open")) {
            context_open_sequence(dpy);
        } else if (!strcmp(a.stage, "context-edit")) {
            context_edit_sequence(dpy);
        } else if (!strcmp(a.stage, "context-save")) {
            modified_key(dpy, XK_Control_L, XK_Return);
        } else if (!strcmp(a.stage, "context-close-confirm")) {
            modified_key(dpy, XK_Control_L, XK_Return);
        } else if (!strcmp(a.stage, "wm-close")) {
            request_wm_delete(dpy, app_window);
        } else if (!strcmp(a.stage, "context-close") ||
                   !strcmp(a.stage, "prompt-close") ||
                   !strcmp(a.stage, "prompt-delete-cancel")) {
            key(dpy, XK_Escape);
        } else if (!strcmp(a.stage, "prompts-open")) {
            setup_prefix(dpy, XK_p, 0);
            const KeySym query[] = { XK_p, XK_r, XK_o, XK_m, XK_p, XK_t, XK_s };
            for (size_t i = 0; i < sizeof(query) / sizeof(query[0]); ++i) key(dpy, query[i]);
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "prompt-new")) {
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "prompt-create-fields")) {
            modified_key(dpy, XK_Control_L, XK_a);
            modified_key(dpy, XK_Shift_L, XK_p);
            const KeySym title_first[] = { XK_r, XK_o, XK_m, XK_p, XK_t };
            for (size_t i = 0; i < sizeof(title_first) / sizeof(title_first[0]); ++i)
                key(dpy, title_first[i]);
            modified_key(dpy, XK_Shift_L, XK_f);
            const KeySym title_last[] = { XK_i, XK_x, XK_t, XK_u, XK_r, XK_e };
            for (size_t i = 0; i < sizeof(title_last) / sizeof(title_last[0]); ++i)
                key(dpy, title_last[i]);
            key(dpy, XK_Tab);
            const KeySym body[] = { XK_r, XK_e, XK_v, XK_i, XK_e, XK_w, XK_space,
                XK_f, XK_i, XK_x, XK_t, XK_u, XK_r, XK_e, XK_space,
                XK_t, XK_e, XK_x, XK_t };
            for (size_t i = 0; i < sizeof(body) / sizeof(body[0]); ++i) key(dpy, body[i]);
        } else if (!strcmp(a.stage, "prompt-save") || !strcmp(a.stage, "prompt-save-edit")) {
            modified_key(dpy, XK_Control_L, XK_Return);
        } else if (!strcmp(a.stage, "prompt-select-first")) {
            key(dpy, XK_Down);
        } else if (!strcmp(a.stage, "prompt-edit")) {
            modified_key(dpy, XK_Control_L, XK_e);
        } else if (!strcmp(a.stage, "prompt-rename")) {
            modified_key(dpy, XK_Control_L, XK_a);
            modified_key(dpy, XK_Shift_L, XK_p);
            const KeySym first[] = { XK_r, XK_o, XK_m, XK_p, XK_t };
            for (size_t i = 0; i < sizeof(first) / sizeof(first[0]); ++i) key(dpy, first[i]);
            modified_key(dpy, XK_Shift_L, XK_r);
            const KeySym last[] = { XK_e, XK_n, XK_a, XK_m, XK_e, XK_d };
            for (size_t i = 0; i < sizeof(last) / sizeof(last[0]); ++i) key(dpy, last[i]);
        } else if (!strcmp(a.stage, "prompt-insert-open")) {
            KeyCode control = XKeysymToKeycode(dpy, XK_Control_L);
            KeyCode shift = XKeysymToKeycode(dpy, XK_Shift_L);
            if (!control || !shift || !XTestFakeKeyEvent(dpy, control, True, CurrentTime) ||
                !XTestFakeKeyEvent(dpy, shift, True, CurrentTime))
                fail("prompt insertion shortcut press failed");
            key(dpy, XK_p);
            if (!XTestFakeKeyEvent(dpy, shift, False, CurrentTime) ||
                !XTestFakeKeyEvent(dpy, control, False, CurrentTime))
                fail("prompt insertion shortcut release failed");
        } else if (!strcmp(a.stage, "prompt-use-first")) {
            key(dpy, XK_Down);
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "prompt-delete")) {
            key(dpy, XK_Delete);
        } else if (!strcmp(a.stage, "prompt-confirm-delete")) {
            key(dpy, XK_Return);
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
        } else if (!strcmp(a.stage, "scrollback-open")) {
            modified_key(dpy, XK_Shift_L, XK_Page_Up);
        } else if (!strcmp(a.stage, "scrollback-wheel")) {
            scrollback_wheel(dpy, app_window);
        } else if (!strcmp(a.stage, "scrollback-top")) {
            key(dpy, XK_Home);
        } else if (!strcmp(a.stage, "scrollback-newer")) {
            modified_key(dpy, XK_Shift_L, XK_Page_Down);
        } else if (!strcmp(a.stage, "scrollback-end")) {
            key(dpy, XK_End);
        } else if (!strcmp(a.stage, "scrollback-close")) {
            key(dpy, XK_Escape);
        } else if (!strcmp(a.stage, "scrollback-probe-input")) {
            key(dpy, XK_x); key(dpy, XK_y); key(dpy, XK_z);
        } else if (!strcmp(a.stage, "scrollback-live-input")) {
            key(dpy, XK_x); key(dpy, XK_y); key(dpy, XK_z);
        } else if (!strcmp(a.stage, "catalog-open-library")) {
            setup_prefix(dpy, XK_d, 0);
        } else if (!strcmp(a.stage, "catalog-close-library")) {
            key(dpy, XK_Escape);
        } else if (!strcmp(a.stage, "catalog-open-save")) {
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "catalog-save")) {
            modified_key(dpy, XK_Control_L, XK_a);
            modified_key(dpy, XK_Shift_L, XK_c);
            const KeySym name[] = { XK_a, XK_t, XK_a, XK_l, XK_o, XK_g };
            for (size_t i = 0; i < sizeof(name) / sizeof(name[0]); ++i) key(dpy, name[i]);
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "catalog-apply")) {
            key(dpy, XK_Down); key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "catalog-undo")) {
            setup_prefix(dpy, XK_z, 0);
        } else if (!strcmp(a.stage, "catalog-delete")) {
            key(dpy, XK_Down);
            modified_key(dpy, XK_Shift_L, XK_Delete); key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "catalog-mutate")) {
            setup_prefix(dpy, XK_r, 0); key(dpy, XK_Right); key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "catalog-rename-tab")) {
            catalog_rename_tab(dpy, app_window);
        } else if (!strcmp(a.stage, "catalog-pane-rename-open") ||
                   !strcmp(a.stage, "catalog-pane-palette-open")) {
            setup_prefix(dpy, XK_p, 0);
        } else if (!strcmp(a.stage, "catalog-pane-rename-action")) {
            const KeySym query[] = { XK_r, XK_e, XK_n, XK_a, XK_m, XK_e };
            for (size_t i = 0; i < sizeof(query) / sizeof(query[0]); ++i) key(dpy, query[i]);
            struct timespec palette_settle = { .tv_sec = 0, .tv_nsec = 400000000 };
            nanosleep(&palette_settle, NULL);
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "catalog-pane-rename-type") ||
                   !strcmp(a.stage, "catalog-pane-palette-query")) {
            if (!strcmp(a.stage, "catalog-pane-rename-type"))
                modified_key(dpy, XK_Control_L, XK_a);
            modified_key(dpy, XK_Shift_L, XK_r);
            const KeySym query[] = { XK_e, XK_v, XK_i, XK_e, XK_w };
            for (size_t i = 0; i < sizeof(query) / sizeof(query[0]); ++i) key(dpy, query[i]);
        } else if (!strcmp(a.stage, "catalog-pane-rename-save")) {
            modified_key(dpy, XK_Control_L, XK_Return);
        } else if (!strcmp(a.stage, "catalog-pane-palette-select")) {
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "catalog-stop-open")) {
            setup_prefix(dpy, XK_p, 0);
        } else if (!strcmp(a.stage, "catalog-stop-action")) {
            const KeySym query[] = { XK_s, XK_t, XK_o, XK_p };
            for (size_t i = 0; i < sizeof(query) / sizeof(query[0]); ++i) key(dpy, query[i]);
            struct timespec palette_settle = { .tv_sec = 0, .tv_nsec = 400000000 };
            nanosleep(&palette_settle, NULL);
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "catalog-stop-cancel")) {
            key(dpy, XK_Escape);
        } else if (!strcmp(a.stage, "catalog-stop-confirm")) {
            key(dpy, XK_Tab);
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "catalog-sessions-open")) {
            setup_prefix(dpy, XK_p, 0);
        } else if (!strcmp(a.stage, "catalog-sessions-action")) {
            const KeySym query[] = { XK_s, XK_e, XK_s, XK_s, XK_i, XK_o, XK_n, XK_s };
            for (size_t i = 0; i < sizeof(query) / sizeof(query[0]); ++i) key(dpy, query[i]);
            struct timespec palette_settle = { .tv_sec = 0, .tv_nsec = 400000000 };
            nanosleep(&palette_settle, NULL);
            XSync(dpy, False);
            capture(dpy, "/artifacts/catalog-sessions-query.ppm");
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "catalog-hidden-stop-review") ||
                   !strcmp(a.stage, "catalog-hidden-stop-confirm")) {
            key(dpy, XK_Tab);
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "catalog-profiles-open")) {
            setup_prefix(dpy, XK_p, 0);
        } else if (!strcmp(a.stage, "catalog-profiles-query")) {
            const KeySym query[] = { XK_p, XK_r, XK_o, XK_f, XK_i, XK_l, XK_e, XK_s };
            for (size_t i = 0; i < sizeof(query) / sizeof(query[0]); ++i) key(dpy, query[i]);
            struct timespec palette_settle = { .tv_sec = 0, .tv_nsec = 400000000 };
            nanosleep(&palette_settle, NULL);
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "catalog-profiles-refresh")) {
            modified_key(dpy, XK_Control_L, XK_r);
        } else if (!strcmp(a.stage, "catalog-profiles-select-first")) {
            key(dpy, XK_Down);
        } else if (!strcmp(a.stage, "catalog-profiles-select-next")) {
            key(dpy, XK_Down);
        } else if (!strcmp(a.stage, "catalog-profiles-pin")) {
            modified_key(dpy, XK_Control_L, XK_p);
        } else if (!strcmp(a.stage, "catalog-profiles-move-up")) {
            modified_key(dpy, XK_Alt_L, XK_Up);
        } else if (!strcmp(a.stage, "catalog-profiles-move-down")) {
            modified_key(dpy, XK_Alt_L, XK_Down);
        } else if (!strcmp(a.stage, "catalog-profiles-close")) {
            key(dpy, XK_Escape);
        } else if (!strcmp(a.stage, "catalog-git-open")) {
            setup_prefix(dpy, XK_p, 0);
        } else if (!strcmp(a.stage, "catalog-git-query")) {
            const KeySym query[] = { XK_g, XK_i, XK_t };
            for (size_t i = 0; i < sizeof(query) / sizeof(query[0]); ++i) key(dpy, query[i]);
            struct timespec palette_settle = { .tv_sec = 0, .tv_nsec = 400000000 };
            nanosleep(&palette_settle, NULL);
            capture(dpy, "/artifacts/catalog-git-query.ppm");
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "catalog-git-refresh")) {
            modified_key(dpy, XK_Control_L, XK_r);
        } else if (!strcmp(a.stage, "catalog-git-diff-first")) {
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "catalog-git-diff-second")) {
            key(dpy, XK_Down);
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "catalog-git-index-apply")) {
            modified_key(dpy, XK_Control_L, XK_s);
        } else if (!strcmp(a.stage, "catalog-git-commit-open") ||
                   !strcmp(a.stage, "catalog-git-commit-confirm")) {
            modified_key(dpy, XK_Control_L, XK_Return);
        } else if (!strcmp(a.stage, "catalog-git-commit-message")) {
            modified_key(dpy, XK_Shift_L, XK_p);
            const KeySym message[] = {
                XK_r, XK_i, XK_v, XK_a, XK_t, XK_e, XK_space,
                XK_r, XK_e, XK_v, XK_i, XK_e, XK_w, XK_e, XK_d, XK_space,
                XK_c, XK_o, XK_m, XK_m, XK_i, XK_t,
            };
            for (size_t i = 0; i < sizeof(message) / sizeof(message[0]); ++i) key(dpy, message[i]);
        } else if (!strcmp(a.stage, "catalog-git-close")) {
            key(dpy, XK_Escape);
        } else if (!strcmp(a.stage, "catalog-close-open")) {
            setup_prefix(dpy, XK_p, 0);
        } else if (!strcmp(a.stage, "catalog-close-query")) {
            const KeySym query[] = { XK_c, XK_l, XK_o, XK_s, XK_e, XK_space,
                                     XK_t, XK_a, XK_b };
            for (size_t i = 0; i < sizeof(query) / sizeof(query[0]); ++i) key(dpy, query[i]);
            struct timespec palette_settle = { .tv_sec = 0, .tv_nsec = 400000000 };
            nanosleep(&palette_settle, NULL);
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "catalog-close-inspect")) {
            modified_key(dpy, XK_Control_L, XK_r);
        } else if (!strcmp(a.stage, "catalog-close-cancel")) {
            key(dpy, XK_Escape);
        } else if (!strcmp(a.stage, "catalog-close-confirm")) {
            modified_key(dpy, XK_Control_L, XK_Return);
        } else if (!strcmp(a.stage, "recipe-select-first")) {
            key(dpy, XK_Down);
        } else if (!strcmp(a.stage, "recipe-open-editor")) {
            modified_key(dpy, XK_Control_L, XK_e);
        } else if (!strcmp(a.stage, "recipe-save-editor")) {
            modified_key(dpy, XK_Control_L, XK_Return);
        } else if (!strcmp(a.stage, "recipe-new-tab")) {
            modified_key(dpy, XK_Control_L, XK_Return);
        } else if (!strcmp(a.stage, "recipe-name-tab")) {
            modified_key(dpy, XK_Control_L, XK_a);
            modified_key(dpy, XK_Shift_L, XK_r);
            const KeySym name[] = { XK_e, XK_c, XK_i, XK_p, XK_e };
            for (size_t i = 0; i < sizeof(name) / sizeof(name[0]); ++i) key(dpy, name[i]);
        } else if (!strcmp(a.stage, "recipe-submit-name")) {
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "recipe-confirm-run")) {
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "recipe-slot-open")) {
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "recipe-slot-dirty")) {
            modified_key(dpy, XK_Control_L, XK_a);
            modified_key(dpy, XK_Shift_L, XK_d);
            const KeySym first[] = { XK_r, XK_a, XK_f, XK_t };
            for (size_t i = 0; i < sizeof(first) / sizeof(first[0]); ++i) key(dpy, first[i]);
            modified_key(dpy, XK_Shift_L, XK_k);
            const KeySym last[] = { XK_e, XK_e, XK_p, XK_s };
            for (size_t i = 0; i < sizeof(last) / sizeof(last[0]); ++i) key(dpy, last[i]);
        } else if (!strcmp(a.stage, "recipe-profile-open")) {
            modified_key(dpy, XK_Control_L, XK_p);
        } else if (!strcmp(a.stage, "recipe-profile-no-match")) {
            modified_key(dpy, XK_Control_L, XK_a);
            const KeySym query[] = { XK_n, XK_o, XK_s, XK_u, XK_c, XK_h, XK_r, XK_e,
                                     XK_c, XK_i, XK_p, XK_e, XK_p, XK_r, XK_o, XK_f,
                                     XK_i, XK_l, XK_e };
            for (size_t i = 0; i < sizeof(query) / sizeof(query[0]); ++i) key(dpy, query[i]);
        } else if (!strcmp(a.stage, "recipe-profile-cancel")) {
            key(dpy, XK_Escape);
        } else if (!strcmp(a.stage, "recipe-profile-select")) {
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "recipe-slot-save")) {
            modified_key(dpy, XK_Control_L, XK_Return);
        } else if (!strcmp(a.stage, "recipe-name-custom-tab")) {
            modified_key(dpy, XK_Control_L, XK_a);
            modified_key(dpy, XK_Shift_L, XK_r);
            const KeySym first[] = { XK_e, XK_c, XK_i, XK_p, XK_e };
            for (size_t i = 0; i < sizeof(first) / sizeof(first[0]); ++i) key(dpy, first[i]);
            modified_key(dpy, XK_Shift_L, XK_c);
            const KeySym last[] = { XK_u, XK_s, XK_t, XK_o, XK_m };
            for (size_t i = 0; i < sizeof(last) / sizeof(last[0]); ++i) key(dpy, last[i]);
        } else if (!strcmp(a.stage, "recipe-custom-confirm")) {
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "profile-open")) {
            setup_prefix(dpy, XK_p, 0);
            const KeySym query[] = { XK_p, XK_r, XK_o, XK_f, XK_i, XK_l, XK_e, XK_s };
            for (size_t i = 0; i < sizeof(query) / sizeof(query[0]); ++i) key(dpy, query[i]);
            key(dpy, XK_Return);
            struct timespec settle = { .tv_sec = 0, .tv_nsec = 300000000 };
            nanosleep(&settle, NULL);
        } else if (!strcmp(a.stage, "profile-new")) {
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "profile-create-fields")) {
            modified_key(dpy, XK_Control_L, XK_a);
            modified_key(dpy, XK_Shift_L, XK_r);
            const KeySym first[] = { XK_e, XK_v, XK_i, XK_e, XK_w, XK_e, XK_r };
            for (size_t i = 0; i < sizeof(first) / sizeof(first[0]); ++i) key(dpy, first[i]);
            modified_key(dpy, XK_Shift_L, XK_f);
            const KeySym second[] = { XK_i, XK_x, XK_t, XK_u, XK_r, XK_e };
            for (size_t i = 0; i < sizeof(second) / sizeof(second[0]); ++i) key(dpy, second[i]);
            key(dpy, XK_Tab);
            const KeySym role[] = { XK_r, XK_e, XK_v, XK_i, XK_e, XK_w, XK_e, XK_r,
                XK_space, XK_p, XK_r, XK_o, XK_f, XK_i, XK_l, XK_e, XK_space,
                XK_r, XK_o, XK_l, XK_e };
            for (size_t i = 0; i < sizeof(role) / sizeof(role[0]); ++i) key(dpy, role[i]);
            key(dpy, XK_Tab);
            const KeySym instructions[] = { XK_p, XK_r, XK_o, XK_f, XK_i, XK_l, XK_e,
                XK_space, XK_i, XK_n, XK_s, XK_t, XK_r, XK_u, XK_c, XK_t, XK_i, XK_o,
                XK_n, XK_space, XK_f, XK_i, XK_x, XK_t, XK_u, XK_r, XK_e };
            for (size_t i = 0; i < sizeof(instructions) / sizeof(instructions[0]); ++i)
                key(dpy, instructions[i]);
            key(dpy, XK_Tab);
            const KeySym model[] = { XK_o, XK_p, XK_e, XK_n, XK_c, XK_o, XK_d, XK_e,
                XK_slash, XK_b, XK_i, XK_g, XK_minus, XK_p, XK_i, XK_c, XK_k, XK_l, XK_e };
            for (size_t i = 0; i < sizeof(model) / sizeof(model[0]); ++i) key(dpy, model[i]);
            modified_key(dpy, XK_Control_L, XK_3);
        } else if (!strcmp(a.stage, "profile-context-off")) {
            modified_key(dpy, XK_Control_L, XK_m);
        } else if (!strcmp(a.stage, "profile-save") || !strcmp(a.stage, "profile-save-edit")) {
            modified_key(dpy, XK_Control_L, XK_Return);
        } else if (!strcmp(a.stage, "profile-select-first")) {
            key(dpy, XK_Down);
        } else if (!strcmp(a.stage, "profile-launch")) {
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "profile-confirm")) {
            modified_key(dpy, XK_Control_L, XK_Return);
        } else if (!strcmp(a.stage, "profile-edit")) {
            modified_key(dpy, XK_Control_L, XK_e);
        } else if (!strcmp(a.stage, "profile-rename")) {
            modified_key(dpy, XK_Control_L, XK_a);
            modified_key(dpy, XK_Shift_L, XK_r);
            const KeySym first[] = { XK_e, XK_v, XK_i, XK_e, XK_w, XK_e, XK_r };
            for (size_t i = 0; i < sizeof(first) / sizeof(first[0]); ++i) key(dpy, first[i]);
            modified_key(dpy, XK_Shift_L, XK_r);
            const KeySym second[] = { XK_e, XK_n, XK_a, XK_m, XK_e, XK_d };
            for (size_t i = 0; i < sizeof(second) / sizeof(second[0]); ++i) key(dpy, second[i]);
            key(dpy, XK_Tab);
            key(dpy, XK_Tab);
            key(dpy, XK_Tab);
            modified_key(dpy, XK_Control_L, XK_a);
            const KeySym model[] = { XK_o, XK_p, XK_e, XK_n, XK_c, XK_o, XK_d, XK_e,
                XK_slash, XK_t, XK_e, XK_s, XK_t, XK_minus, XK_o, XK_t, XK_h, XK_e,
                XK_r, XK_minus, XK_m, XK_o, XK_d, XK_e, XK_l };
            for (size_t i = 0; i < sizeof(model) / sizeof(model[0]); ++i) key(dpy, model[i]);
        } else if (!strcmp(a.stage, "profile-delete")) {
            key(dpy, XK_Delete);
        } else if (!strcmp(a.stage, "profile-cancel-delete")) {
            key(dpy, XK_Escape);
        } else if (!strcmp(a.stage, "profile-confirm-delete")) {
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "setup-open")) {
            setup_click(dpy, app_window, 885, 435);
        } else if (!strcmp(a.stage, "setup-create")) {
            setup_create_sequence(dpy);
        } else if (!strcmp(a.stage, "setup-shell")) {
            wait_workspace_bar(dpy, app_window);
            setup_click(dpy, app_window, 1164, 57);
        } else if (!strcmp(a.stage, "setup-launcher")) {
            wait_workspace_bar(dpy, app_window);
            setup_click(dpy, app_window, 1108, 57);
        } else if (!strcmp(a.stage, "setup-codex")) {
            setup_click(dpy, app_window, 490, 177);
        } else if (!strcmp(a.stage, "custom-open")) {
            setup_prefix(dpy, XK_p, 0);
            const KeySym query[] = { XK_c, XK_u, XK_s, XK_t, XK_o, XK_m, XK_space,
                XK_c, XK_l, XK_i };
            for (size_t i = 0; i < sizeof(query) / sizeof(query[0]); ++i) key(dpy, query[i]);
            struct timespec settle = { .tv_sec = 0, .tv_nsec = 400000000 };
            nanosleep(&settle, NULL);
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "custom-fields")) {
            modified_key(dpy, XK_Control_L, XK_a);
            custom_ascii(dpy, "LiteralFixture");
            key(dpy, XK_Tab);
            custom_ascii(dpy, "/artifacts/tools/custom-cli");
            key(dpy, XK_Tab);
            custom_ascii(dpy, "--label \"two words\" '$(literal);'");
        } else if (!strcmp(a.stage, "custom-save")) {
            modified_key(dpy, XK_Control_L, XK_Return);
        } else if (!strcmp(a.stage, "custom-select")) {
            key(dpy, XK_Down);
            key(dpy, XK_Return);
            struct timespec settle = { .tv_sec = 0, .tv_nsec = 400000000 };
            nanosleep(&settle, NULL);
        } else if (!strcmp(a.stage, "custom-reopen-profiles")) {
            setup_prefix(dpy, XK_p, 0);
            const KeySym query[] = { XK_p, XK_r, XK_o, XK_f, XK_i, XK_l, XK_e, XK_s };
            for (size_t i = 0; i < sizeof(query) / sizeof(query[0]); ++i) key(dpy, query[i]);
            struct timespec settle = { .tv_sec = 0, .tv_nsec = 400000000 };
            nanosleep(&settle, NULL);
            key(dpy, XK_Return);
            nanosleep(&settle, NULL);
        } else if (!strcmp(a.stage, "custom-review-enter")) {
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "custom-review-cancel")) {
            key(dpy, XK_Escape);
        } else if (!strcmp(a.stage, "custom-confirm")) {
            modified_key(dpy, XK_Control_L, XK_Return);
        } else if (!strcmp(a.stage, "setup-compact")) {
            XResizeWindow(dpy, app_window, 675, 734); XSync(dpy,False);
        } else if (!strcmp(a.stage, "setup-wide")) {
            XResizeWindow(dpy, app_window, 1280, 800); XSync(dpy,False);
        } else if (!strcmp(a.stage, "setup-live")) {
            const KeySym command[] = { XK_e, XK_c, XK_h, XK_o, XK_space,
                XK_p, XK_i, XK_g, XK_a, XK_d, XK_e, XK_g, XK_r, XK_i, XK_d, XK_l, XK_i, XK_v, XK_e };
            for (size_t i = 0; i < sizeof(command) / sizeof(command[0]); ++i)
                key(dpy, command[i]);
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "setup-close")) {
            key(dpy, XK_Escape);
        } else if (!strcmp(a.stage, "setup-focus-left")) {
            setup_prefix(dpy, XK_h, 0);
        } else if (!strcmp(a.stage, "setup-split-right")) {
            wait_workspace_bar(dpy, app_window);
            setup_click(dpy, app_window, 924, 92);
        } else if (!strcmp(a.stage, "setup-split-down")) {
            setup_prefix(dpy, XK_s, 0);
        } else if (!strcmp(a.stage, "opencode-input")) {
            opencode_input_sequence(dpy, app_window);
        } else if (!strcmp(a.stage, "controls-focus-toggle")) {
            modified_key(dpy, XK_Control_L, XK_f);
        } else if (!strcmp(a.stage, "controls-focus-right")) {
            modified_key(dpy, XK_Control_L, XK_Right);
        } else if (!strcmp(a.stage, "controls-alt-focus-toggle")) {
            modified_key(dpy, XK_Alt_L, XK_f);
        } else if (!strcmp(a.stage, "controls-swap")) {
            controls_drag(dpy, app_window, 1, 0);
        } else if (!strcmp(a.stage, "controls-move")) {
            controls_drag(dpy, app_window, 1, 0);
        } else if (!strcmp(a.stage, "controls-resize-cancel")) {
            controls_drag(dpy, app_window, 3, 1);
        } else if (!strcmp(a.stage, "controls-resize-commit")) {
            controls_drag(dpy, app_window, 3, 0);
        } else if (!strcmp(a.stage, "controls-close")) {
            modified_key(dpy, XK_Control_L, XK_q);
        } else if (!strcmp(a.stage, "controls-open")) {
            setup_click(dpy, app_window, 1235, 20);
        } else if (!strcmp(a.stage, "controls-select-alt")) {
            key(dpy, XK_2);
        } else if (!strcmp(a.stage, "controls-select-control")) {
            key(dpy, XK_1);
        } else if (!strcmp(a.stage, "controls-select-disabled")) {
            key(dpy, XK_5);
        } else if (!strcmp(a.stage, "appearance-open")) {
            setup_prefix(dpy, XK_p, 0);
            const KeySym query[] = { XK_a, XK_p, XK_p, XK_e, XK_a,
                                     XK_r, XK_a, XK_n, XK_c, XK_e };
            for (size_t i = 0; i < sizeof(query) / sizeof(query[0]); ++i) key(dpy, query[i]);
            struct timespec palette_settle = { .tv_sec = 0, .tv_nsec = 400000000 };
            nanosleep(&palette_settle, NULL);
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "appearance-light")) {
            key(dpy, XK_2);
        } else if (!strcmp(a.stage, "appearance-motion")) {
            key(dpy, XK_r);
        } else if (!strcmp(a.stage, "appearance-dark")) {
            key(dpy, XK_1);
        } else if (!strcmp(a.stage, "appearance-terminal-dark")) {
            key(dpy, XK_3);
        } else if (!strcmp(a.stage, "appearance-terminal-light")) {
            key(dpy, XK_4);
        } else if (!strcmp(a.stage, "appearance-terminal-follow")) {
            key(dpy, XK_5);
        } else if (!strcmp(a.stage, "appearance-font-increase")) {
            key(dpy, XK_equal);
        } else if (!strcmp(a.stage, "appearance-font-decrease")) {
            key(dpy, XK_minus);
        } else if (!strcmp(a.stage, "appearance-font-reset")) {
            key(dpy, XK_0);
        } else if (!strcmp(a.stage, "appearance-reload")) {
            modified_key(dpy, XK_Control_L, XK_r);
        } else if (!strcmp(a.stage, "appearance-close")) {
            key(dpy, XK_Escape);
        } else if (!strcmp(a.stage, "pointer-hover-left")) {
            pointer_hover(dpy, app_window, 485, 250, 0);
        } else if (!strcmp(a.stage, "pointer-hover-header")) {
            pointer_hover(dpy, app_window, 485, 90, 0);
            capture(dpy, a.output);
        } else if (!strcmp(a.stage, "pointer-hover-right")) {
            pointer_hover(dpy, app_window, 1015, 250, 0);
        } else if (!strcmp(a.stage, "pointer-hover-input")) {
            pointer_hover(dpy, app_window, 485, 250, 1);
        } else if (!strcmp(a.stage, "pointer-no-motion-release")) {
            pointer_no_motion_begin(dpy, app_window);
            struct timespec settle = { .tv_sec = 0, .tv_nsec = 200000000 };
            nanosleep(&settle, NULL);
            capture(dpy, a.output);
            puts("HELD"); fflush(stdout);
            char response[32];
            if (!fgets(response, sizeof(response), stdin) || strcmp(response, "RELEASE\n"))
                fail("pointer no-motion probe accepts only fixed button release");
            pointer_drag_release(dpy, 0);
        } else if (!strcmp(a.stage, "pointer-hover-source")) {
            pointer_hover(dpy, app_window, 350, 250, 0);
        } else if (!strcmp(a.stage, "pointer-file-drop")) {
            pointer_file_drop(dpy, c, app_window, a.app, URI_PAYLOAD_VALID);
            capture(dpy, a.output);
        } else if (!strcmp(a.stage, "pointer-file-drop-invalid-uri")) {
            pointer_file_drop(dpy, c, app_window, a.app, URI_PAYLOAD_NONFILE);
            capture(dpy, a.output);
        } else if (!strcmp(a.stage, "pointer-file-drop-truncated")) {
            pointer_file_drop(dpy, c, app_window, a.app, URI_PAYLOAD_TRUNCATED);
            capture(dpy, a.output);
        } else if (!strcmp(a.stage, "pointer-file-drop-cancel")) {
            key(dpy, XK_Escape);
        } else if (!strcmp(a.stage, "pointer-file-drop-confirm")) {
            modified_key(dpy, XK_Control_L, XK_Return);
        } else if (!strcmp(a.stage, "pointer-clipboard-paste")) {
            pointer_clipboard_paste(dpy, c, a.app, a.fcitx);
        } else if (!strcmp(a.stage, "pointer-clipboard-enter")) {
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "pointer-clipboard-cancel")) {
            key(dpy, XK_Escape);
        } else if (!strcmp(a.stage, "pointer-clipboard-confirm")) {
            modified_key(dpy, XK_Control_L, XK_Return);
        } else if (!strcmp(a.stage, "pointer-subthreshold-hold")) {
            KeyCode control = XKeysymToKeycode(dpy, XK_Control_L);
            if (!control || !XTestFakeKeyEvent(dpy, control, True, CurrentTime))
                fail("small-motion drag modifier press failed");
            pointer_position(dpy, app_window, 350, 250);
            if (!XTestFakeButtonEvent(dpy, 1, True, CurrentTime))
                fail("small-motion drag button press failed");
            pointer_position(dpy, app_window, 355, 250);
            struct timespec settle = { .tv_sec = 0, .tv_nsec = 250000000 };
            nanosleep(&settle, NULL);
            XSync(dpy, False);
            capture(dpy, a.output);
            puts("HELD"); fflush(stdout);
            char response[32];
            if (!fgets(response, sizeof(response), stdin) || strcmp(response, "CANCEL\n"))
                fail("small-motion pointer preview accepts only fixed cancel release");
            pointer_drag_release(dpy, 1);
        } else if (!strcmp(a.stage, "pointer-focus-toggle")) {
            modified_key(dpy, XK_Control_L, XK_f);
        } else if (!strcmp(a.stage, "pointer-keyboard-away")) {
            setup_prefix(dpy, XK_h, 0);
        } else if (!strcmp(a.stage, "pointer-move-right-near")) {
            pointer_hover(dpy, app_window, 1000, 250, 0);
        } else if (!strcmp(a.stage, "pointer-hold-preview")) {
            pointer_drag_begin(dpy, app_window);
            struct timespec settle = { .tv_sec = 0, .tv_nsec = 250000000 };
            nanosleep(&settle, NULL);
            XSync(dpy, False);
            capture(dpy, a.output);
            puts("HELD"); fflush(stdout);
            char response[32];
            if (!fgets(response, sizeof(response), stdin) || strcmp(response, "CANCEL\n"))
                fail("pointer preview accepts only fixed cancel release");
            pointer_drag_release(dpy, 1);
        } else if (!strcmp(a.stage, "pointer-focus-mode-preview")) {
            pointer_drag_begin(dpy, app_window);
            struct timespec settle = { .tv_sec = 0, .tv_nsec = 250000000 };
            nanosleep(&settle, NULL);
            XSync(dpy, False);
            capture(dpy, a.output);
            puts("HELD"); fflush(stdout);
            char response[32];
            if (!fgets(response, sizeof(response), stdin) || strcmp(response, "CANCEL\n"))
                fail("focus-mode pointer preview accepts only fixed cancel release");
            pointer_drag_release(dpy, 1);
        } else if (!strcmp(a.stage, "pointer-drop")) {
            pointer_drag_begin(dpy, app_window);
            struct timespec settle = { .tv_sec = 0, .tv_nsec = 250000000 };
            nanosleep(&settle, NULL);
            pointer_drag_release(dpy, 0);
        } else if (!strcmp(a.stage, "pointer-drop-gap")) {
            pointer_gap_drag(dpy, app_window);
            struct timespec settle = { .tv_sec = 0, .tv_nsec = 250000000 };
            nanosleep(&settle, NULL);
            pointer_drag_release(dpy, 0);
        } else if (!strcmp(a.stage, "pointer-create-transfer-tab")) {
            setup_prefix(dpy, XK_t, 0);
            struct timespec settle = { .tv_sec = 0, .tv_nsec = 150000000 };
            nanosleep(&settle, NULL);
            modified_key(dpy, XK_Shift_L, XK_t);
            const KeySym name[] = { XK_r, XK_a, XK_n, XK_s, XK_f, XK_e, XK_r };
            for (size_t i = 0; i < sizeof(name) / sizeof(name[0]); ++i) key(dpy, name[i]);
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "pointer-select-first-tab")) {
            modified_key(dpy, XK_Control_L, XK_1);
        } else if (!strcmp(a.stage, "pointer-hotkey-transfer")) {
            control_shift_key(dpy, XK_2);
        } else if (!strcmp(a.stage, "pointer-picker-transfer")) {
            setup_prefix(dpy, XK_m, 0);
            const KeySym query[] = { XK_t, XK_r, XK_a, XK_n, XK_s, XK_f, XK_e, XK_r };
            for (size_t i = 0; i < sizeof(query) / sizeof(query[0]); ++i) key(dpy, query[i]);
            key(dpy, XK_Down);
            key(dpy, XK_Up);
            modified_key(dpy, XK_Control_L, XK_r);
        } else if (!strcmp(a.stage, "pointer-picker-confirm")) {
            key(dpy, XK_Return);
        } else if (!strcmp(a.stage, "pointer-tab-drop-cancel") ||
                   !strcmp(a.stage, "pointer-tab-drop") ||
                   !strcmp(a.stage, "pointer-tab-drop-populated-preview")) {
            int cancel = !strcmp(a.stage, "pointer-tab-drop-cancel");
            pointer_tab_drag_begin(dpy, app_window);
            struct timespec settle = { .tv_sec = 0, .tv_nsec = 300000000 };
            nanosleep(&settle, NULL);
            XSync(dpy, False);
            capture(dpy, a.output);
            puts("HELD"); fflush(stdout);
            char response[32];
            const char *expected = cancel ? "CANCEL\n" : "RELEASE\n";
            if (!fgets(response, sizeof(response), stdin) || strcmp(response, expected))
                fail("tab-drop probe accepts only its fixed release response");
            pointer_drag_release(dpy, cancel);
        } else if (!strcmp(a.stage, "pointer-undo")) {
            setup_prefix(dpy, XK_z, 0);
        } else if (!strcmp(a.stage, "pointer-selection-hold")) {
            pointer_selection_hold(dpy, app_window, a.output);
        } else if (!strcmp(a.stage, "pointer-ime-input")) {
            keyboard_group(dpy, 0);
            const KeySym preedit[] = { XK_n, XK_i, XK_h, XK_a, XK_o };
            for (size_t i = 0; i < sizeof(preedit) / sizeof(preedit[0]); ++i) key(dpy, preedit[i]);
            struct timespec settle = { .tv_sec = 0, .tv_nsec = 250000000 };
            nanosleep(&settle, NULL);
            XSync(dpy, False);
            capture(dpy, a.output);
            pointer_position(dpy, app_window, 1015, 250);
            XSync(dpy, False);
        } else if (!strcmp(a.stage, "pointer-ime-cancel")) {
            key(dpy, XK_Escape);
        } else if (!strcmp(a.stage, "pointer-dialog-hover")) {
            pointer_hover(dpy, app_window, 1015, 250, 0);
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
                            !strcmp(a.stage, "scrollback-open") ? "2" :
                            !strcmp(a.stage, "scrollback-wheel") ? "0" :
                            !strcmp(a.stage, "scrollback-newer") ? "2" :
                            !strcmp(a.stage, "scrollback-probe-input") ? "3" :
                            !strcmp(a.stage, "scrollback-live-input") ? "3" :
                            !strcmp(a.stage, "opencode-input") ? "16" :
                            !strcmp(a.stage, "controls-focus-toggle") ? "2" :
                            !strcmp(a.stage, "controls-focus-right") ? "2" :
                            !strcmp(a.stage, "controls-alt-focus-toggle") ? "2" :
                            !strcmp(a.stage, "controls-swap") ? "2" :
                            !strcmp(a.stage, "controls-resize-cancel") ? "2" :
                            !strcmp(a.stage, "controls-resize-commit") ? "2" :
                            !strcmp(a.stage, "controls-close") ? "2" :
                            !strcmp(a.stage, "controls-select-alt") ? "1" :
                            !strcmp(a.stage, "controls-select-control") ? "1" :
                            !strcmp(a.stage, "controls-select-disabled") ? "1" :
                            !strcmp(a.stage, "appearance-open") ? "15" :
                            !strcmp(a.stage, "appearance-light") ? "1" :
                            !strcmp(a.stage, "appearance-motion") ? "1" :
                            !strcmp(a.stage, "appearance-dark") ? "1" :
                            !strcmp(a.stage, "appearance-terminal-dark") ? "1" :
                            !strcmp(a.stage, "appearance-terminal-light") ? "1" :
                            !strcmp(a.stage, "appearance-terminal-follow") ? "1" :
                            !strcmp(a.stage, "appearance-font-increase") ? "1" :
                            !strcmp(a.stage, "appearance-font-decrease") ? "1" :
                            !strcmp(a.stage, "appearance-font-reset") ? "1" :
                            !strcmp(a.stage, "appearance-reload") ? "2" :
                            !strcmp(a.stage, "appearance-close") ? "1" :
                            !strcmp(a.stage, "pointer-no-motion-release") ? "0" :
                            !strcmp(a.stage, "pointer-focus-toggle") ? "2" :
                            !strcmp(a.stage, "pointer-hover-input") ? "11" :
                            !strcmp(a.stage, "pointer-ime-input") ? "5" :
                            !strcmp(a.stage, "pointer-undo") ? "2" :
                            !strcmp(a.stage, "setup-open") ? "1" :
                            !strcmp(a.stage, "setup-create") ? "5" :
                            !strcmp(a.stage, "setup-shell") ? "1" :
                            !strcmp(a.stage, "setup-live") ? "20" :
                            !strcmp(a.stage, "catalog-open-library") ? "4" :
                            !strcmp(a.stage, "catalog-close-library") ? "1" :
                            !strcmp(a.stage, "catalog-open-save") ? "1" :
                            !strcmp(a.stage, "catalog-save") ? "11" :
                            !strcmp(a.stage, "catalog-apply") ? "2" :
                            !strcmp(a.stage, "catalog-undo") ? "4" :
                            !strcmp(a.stage, "catalog-delete") ? "3" :
                            !strcmp(a.stage, "catalog-mutate") ? "6" :
                            !strcmp(a.stage, "catalog-rename-tab") ? "14" :
                            !strcmp(a.stage, "catalog-pane-rename-open") ? "4" :
                            !strcmp(a.stage, "catalog-pane-rename-action") ? "7" :
                            !strcmp(a.stage, "catalog-pane-rename-type") ? "9" :
                            !strcmp(a.stage, "catalog-pane-rename-save") ? "2" :
                            !strcmp(a.stage, "catalog-pane-palette-open") ? "4" :
                            !strcmp(a.stage, "catalog-pane-palette-query") ? "6" :
                            !strcmp(a.stage, "catalog-pane-palette-select") ? "1" :
                            !strcmp(a.stage, "catalog-stop-open") ? "4" :
                            !strcmp(a.stage, "catalog-stop-action") ? "5" :
                            !strcmp(a.stage, "catalog-stop-cancel") ? "1" :
                            !strcmp(a.stage, "catalog-stop-confirm") ? "2" :
                            !strcmp(a.stage, "catalog-sessions-open") ? "4" :
                            !strcmp(a.stage, "catalog-sessions-action") ? "9" :
                            !strcmp(a.stage, "catalog-hidden-stop-review") ? "2" :
                            !strcmp(a.stage, "catalog-hidden-stop-confirm") ? "2" :
                            !strcmp(a.stage, "catalog-profiles-open") ? "4" :
                            !strcmp(a.stage, "catalog-profiles-query") ? "9" :
                            !strcmp(a.stage, "catalog-profiles-refresh") ? "2" :
                            !strcmp(a.stage, "catalog-profiles-select-first") ? "1" :
                            !strcmp(a.stage, "catalog-profiles-select-next") ? "1" :
                            !strcmp(a.stage, "catalog-profiles-pin") ? "2" :
                            !strcmp(a.stage, "catalog-profiles-move-up") ? "2" :
                            !strcmp(a.stage, "catalog-profiles-move-down") ? "2" :
                            !strcmp(a.stage, "catalog-profiles-close") ? "1" :
                            !strcmp(a.stage, "catalog-git-open") ? "4" :
                            !strcmp(a.stage, "catalog-git-query") ? "8" :
                            !strcmp(a.stage, "catalog-git-refresh") ? "2" :
                            !strcmp(a.stage, "catalog-git-diff-first") ? "1" :
                            !strcmp(a.stage, "catalog-git-diff-second") ? "2" :
                            !strcmp(a.stage, "catalog-git-index-apply") ? "2" :
                            !strcmp(a.stage, "catalog-git-commit-open") ? "2" :
                            !strcmp(a.stage, "catalog-git-commit-message") ? "23" :
                            !strcmp(a.stage, "catalog-git-commit-confirm") ? "2" :
                            !strcmp(a.stage, "catalog-git-close") ? "1" :
                            !strcmp(a.stage, "catalog-close-open") ? "4" :
                            !strcmp(a.stage, "catalog-close-query") ? "10" :
                            !strcmp(a.stage, "catalog-close-inspect") ? "2" :
                            !strcmp(a.stage, "catalog-close-cancel") ? "1" :
                            !strcmp(a.stage, "catalog-close-confirm") ? "2" :
                            !strcmp(a.stage, "recipe-select-first") ? "1" :
                            !strcmp(a.stage, "recipe-open-editor") ? "2" :
                            !strcmp(a.stage, "recipe-save-editor") ? "2" :
                            !strcmp(a.stage, "recipe-new-tab") ? "2" :
                            !strcmp(a.stage, "recipe-name-tab") ? "7" :
                            !strcmp(a.stage, "recipe-submit-name") ? "1" :
                            !strcmp(a.stage, "recipe-confirm-run") ? "1" :
                            !strcmp(a.stage, "recipe-slot-open") ? "1" :
                            !strcmp(a.stage, "recipe-slot-dirty") ? "14" :
                            !strcmp(a.stage, "recipe-profile-open") ? "2" :
                            !strcmp(a.stage, "recipe-profile-no-match") ? "21" :
                            !strcmp(a.stage, "recipe-profile-cancel") ? "1" :
                            !strcmp(a.stage, "recipe-profile-select") ? "1" :
                            !strcmp(a.stage, "recipe-slot-save") ? "2" :
                            !strcmp(a.stage, "recipe-name-custom-tab") ? "16" :
                            !strcmp(a.stage, "recipe-custom-confirm") ? "1" :
                            !strcmp(a.stage, "profile-open") ? "11" :
                            !strcmp(a.stage, "profile-new") ? "1" :
                            !strcmp(a.stage, "profile-create-fields") ? "87" :
                            !strcmp(a.stage, "profile-context-off") ? "2" :
                            !strcmp(a.stage, "profile-save") ? "2" :
                            !strcmp(a.stage, "profile-select-first") ? "1" :
                            !strcmp(a.stage, "profile-launch") ? "1" :
                            !strcmp(a.stage, "profile-confirm") ? "2" :
                            !strcmp(a.stage, "profile-edit") ? "2" :
                            !strcmp(a.stage, "profile-rename") ? "46" :
                            !strcmp(a.stage, "profile-save-edit") ? "2" :
                            !strcmp(a.stage, "profile-delete") ? "1" :
                            !strcmp(a.stage, "profile-cancel-delete") ? "1" :
                            !strcmp(a.stage, "profile-confirm-delete") ? "1" :
                            !strcmp(a.stage, "prompt-create-fields") ? "34" :
                            !strcmp(a.stage, "prompt-rename") ? "15" :
                            !strcmp(a.stage, "context-edit") ? "7" :
                            !strcmp(a.stage, "wm-close") ? "0" : "1";
        if (!strcmp(a.stage, "pointer-clipboard-paste")) {
            printf("{\"stage\":\"%s\",\"app_pid\":%ld,\"synthetic_keystrokes\":%s,"
                   "\"selection_transfer\":true,\"payload_bytes\":%zu}\n",
                   a.stage, (long)a.app, count, sizeof(FIXED_CLIPBOARD_TEXT) - 1);
        } else {
            printf("{\"stage\":\"%s\",\"app_pid\":%ld,\"synthetic_keystrokes\":%s}\n",
                   a.stage, (long)a.app, count);
        }
    }
    XSync(dpy, False); xcb_disconnect(c); XCloseDisplay(dpy); return 0;
}
