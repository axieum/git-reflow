/// Adds two integers together.
///
/// # Arguments
/// * `a` - first term
/// * `b` - second term
///
/// # Returns
/// The sum of the two terms.
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        assert_eq!(5, add(2, 3));
    }
}
