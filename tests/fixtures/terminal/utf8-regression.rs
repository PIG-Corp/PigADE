//! Standalone upstream vte 0.15.0 regression reproducer.
//! Dependency: vte = { version = "=0.15.0", features = ["ansi"] }
//! Fails on upstream; should pass with PigADE's reviewed consumed-byte fix.
use vte::ansi::{Handler, Processor};
#[derive(Default)]
struct Sink(String);
impl Handler for Sink {
    fn input(&mut self, c: char) {
        self.0.push(c);
    }
}
fn main() {
    let input = "é 界".as_bytes();
    let mut whole: Processor = Processor::new();
    let mut expected = Sink::default();
    whole.advance(&mut expected, input);
    let mut split: Processor = Processor::new();
    let mut actual = Sink::default();
    split.advance(&mut actual, &input[..1]);
    split.advance(&mut actual, &input[1..]);
    println!("input={input:02x?}; whole={:?}; split={:?}", expected.0, actual.0);
    assert_eq!(expected.0, "é 界");
    assert_eq!(actual.0, expected.0, "partial UTF-8 swallowed valid ASCII");
}
