import headless from '@xterm/headless';
import serializer from '@xterm/addon-serialize';
const { Terminal } = headless;
const { SerializeAddon } = serializer;

// Only this module (in the host or isolated test model) consumes ANSI bytes.
export function createModel(cols = 80, rows = 24, scrollback = 10_000) {
  const terminal = new Terminal({ cols, rows, scrollback, allowProposedApi: true });
  const addon = new SerializeAddon();
  terminal.loadAddon(addon);
  return { terminal, addon };
}
export function write(terminal, bytes) {
  return new Promise(resolve => terminal.write(bytes, resolve));
}
export async function apply(terminal, event) {
  if (event.kind === 'output') await write(terminal, Buffer.from(event.bytes, 'base64'));
  else if (event.kind === 'resize') terminal.resize(event.cols, event.rows);
  else throw new Error(`Unsupported replay event: ${event.kind}`);
}
function cellValue(cell) {
  return {
    chars: cell.getChars(), code: cell.getCode(), width: cell.getWidth(),
    fgMode: cell.getFgColorMode(), fg: cell.getFgColor(),
    bgMode: cell.getBgColorMode(), bg: cell.getBgColor(),
    bold: !!cell.isBold(), italic: !!cell.isItalic(), dim: !!cell.isDim(),
    underline: !!cell.isUnderline(), blink: !!cell.isBlink(), inverse: !!cell.isInverse(),
    invisible: !!cell.isInvisible(), strike: !!cell.isStrikethrough(), overline: !!cell.isOverline()
  };
}
function bufferValue(buffer, viewportOnly = false) {
  const lines = [];
  const first = viewportOnly ? buffer.baseY : 0;
  for (let y = first; y < buffer.length; y++) {
    const line = buffer.getLine(y);
    const cells = [];
    for (let x = 0; x < line.length; x++) cells.push(cellValue(line.getCell(x)));
    lines.push({ wrapped: line.isWrapped, cells });
  }
  return { cursorX: buffer.cursorX, cursorY: buffer.cursorY, baseY: buffer.baseY,
    viewportY: buffer.viewportY, lines };
}
export function snapshot(terminal) {
  return { cols: terminal.cols, rows: terminal.rows, active: terminal.buffer.active.type,
    modes: { ...terminal.modes }, normal: bufferValue(terminal.buffer.normal),
    alternate: bufferValue(terminal.buffer.alternate) };
}
export function frame(terminal) {
  const buffer = terminal.buffer.active;
  const value = bufferValue(buffer, true);
  return { cols: terminal.cols, rows: terminal.rows, active: buffer.type,
    cursorX: buffer.cursorX, cursorY: buffer.cursorY, modes: { ...terminal.modes },
    lines: value.lines.slice(0, terminal.rows) };
}
export function output(bytes) {
  return { kind: 'output', bytes: Buffer.from(bytes).toString('base64') };
}
