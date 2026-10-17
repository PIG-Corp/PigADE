# Actual-app X11 keyboard and IME regression evidence

Both failed baseline runs use release app b4d63952… (pre Alt fix); the second
retains bounded raw synthetic fixture hex and shows only ESC+x missing. The
fixed app f9fea286… includes Alt encoding plus the separately tested command
failure gate. It receives all31 expected keyboard bytes exactly once. The raw
PTY child is controlled synthetic input only; no provider/auth/session history
was accessed. The subsequent Pinyin regression uses the same fixed app and
host, receives exactly6 UTF8 bytes after commit and none before, with no extras.

The source manifest pins the dirty app sources and host source commit used for
these builds. It is not a claim that HEAD later remained unchanged. Per-run
config/results contain exact executable/helper/SDK/font hashes and private
namespace/display/window identities. PPM captures were converted losslessly to
PNG; root inspected keyboard ACK and current Pinyin preedit/final captures.
IME candidates are a native Fcitx popup, not proof of inline GPUI preedit.
No executable, runtime DB/journal, provider data or Xauthority cookie is copied.
