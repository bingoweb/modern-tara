fn main() {
    let n: u32 = 4;
    println!("{}", n); // UNCHANGED
    let s: &str = "xab";
    println!("{}", s); // OLD:uninlined_format_args:modern
    println!("{s}"); // DECOY
    if n % 2 == 0 { // OLD:manual_is_multiple_of:needs-newer
        println!("even");
    }
    let t = s.strip_prefix("x").unwrap_or(s); // OLD:rust-strip-unwrap-or-trim:needs-newer
    let u = s.trim_end_matches('b'); // DECOY
    println!("{t}{u}");
}
