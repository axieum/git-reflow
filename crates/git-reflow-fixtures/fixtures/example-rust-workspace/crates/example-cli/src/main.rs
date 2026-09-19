use example_api;

/// The main entrypoint of the `example-rust-workspace` command-line interface.
fn main() {
    println!("Hello, world! 5 + 9 is {}", example_api::add(5, 9));
}
