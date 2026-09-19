// A whole markdown document in, the site's body HTML out -- so the Python can
// be compared against it without either of them writing a page.
use std::io::Read;
fn main() {
    let idt = |s: &str| s.to_string();
    let mut md = String::new();
    std::io::stdin().read_to_string(&mut md).unwrap();
    print!("{}", radbeeper::site::render(&md, &idt));
}
