use forge_task::Task;
use std::convert::Infallible;

struct CharacterCount;
impl Task for CharacterCount {
    type Input = String;
    type Output = usize;
    type Error = Infallible;
    fn run(&self, input: String) -> Result<usize, Infallible> {
        Ok(input.chars().count())
    }
}

#[test]
fn pure_task_needs_no_client_context_or_envelope() {
    let task: &dyn Task<Input = String, Output = usize, Error = Infallible> = &CharacterCount;
    assert_eq!(task.run("é!".into()), Ok(2));
}

#[test]
fn pure_task_preserves_its_own_typed_error() {
    #[derive(Debug, PartialEq, Eq)]
    struct Overflow;
    struct Add;
    impl Task for Add {
        type Input = (u8, u8);
        type Output = u8;
        type Error = Overflow;
        fn run(&self, (a, b): (u8, u8)) -> Result<u8, Overflow> {
            a.checked_add(b).ok_or(Overflow)
        }
    }
    assert_eq!(Add.run((255, 1)), Err(Overflow));
}
