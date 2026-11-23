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
    quiet(s);
    shorter(&[1], true, "x");
}

#[allow(clippy::uninlined_format_args)]
fn quiet(s: &str) {
    println!("{}", s); // OLD:uninlined_format_args:modern
}

fn double(x: i32) -> i32 {
    x * 2
}

fn side(x: i32) {
    println!("{x}");
}

fn shorter(v: &[i32], flag: bool, name: &str) -> usize {
    let first = v.iter().filter(|x| **x > 1).next(); // OLD:filter_next:modern
    let found = v.iter().find(|x| **x > 1); // DECOY
    let copied: Vec<i32> = v.iter().map(|x| *x).collect(); // OLD:map_clone:modern
    let mut i = 0;
    for x in v { // OLD:explicit_counter_loop:modern
        println!("{x} {i}");
        i += 1;
    }
    let empty = v.len() == 0; // OLD:len_zero:modern
    let none = v.is_empty(); // DECOY
    let same = flag == true; // OLD:bool_comparison:modern
    let ok = if flag { true } else { false }; // OLD:needless_bool:modern
    let total = v.iter().fold(0, |acc, x| acc + x); // OLD:unnecessary_fold:modern
    let mut it = v.iter();
    let head = it.nth(0); // OLD:iter_nth_zero:modern
    let split = name.split("x").count(); // OLD:single_char_pattern:modern
    let owned = name.to_owned().to_string(); // OLD:implicit_clone:modern
    let lazy = Some(1).unwrap_or_else(|| 3); // OLD:unnecessary_lazy_evaluations:modern
    let doubled: Vec<i32> = v.iter().copied().map(|x| double(x)).collect(); // OLD:redundant_closure:modern
    Some(2).map(|q| side(q)); // OLD:redundant_closure:modern
    if flag {
        side(1);
    } else /* OLD:collapsible_else_if:modern */ {
        if ok {
            side(2);
        }
    }
    println!("{first:?}{found:?}{copied:?}{empty}{none}{same}{total}{head:?}{split}{owned}{lazy}{doubled:?}");
    v.len()
}
