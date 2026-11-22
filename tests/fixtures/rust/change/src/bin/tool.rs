fn main() {
    let owned = String::from("xy");
    let w = owned.strip_prefix('x').unwrap_or(&owned); // OLD:rust-strip-unwrap-or-trim:needs-newer
    println!("{}", w); // OLD:uninlined_format_args:modern
}
