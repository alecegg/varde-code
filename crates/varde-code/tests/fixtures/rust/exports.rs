// Export fixtures: `pub` items at the top level yield an Export entity
// alongside their primary entity (narrow: visibility modifier on a top-level
// function/struct/enum/trait/const). Items nested inside `mod` are not
// top-level and do not yield Export entities.

pub fn public_function() -> i32 {
    42
}

pub struct PublicStruct {
    pub field: i32,
}

pub enum PublicEnum {
    A,
}

pub trait PublicTrait {
    fn method(&self);
}

pub const LIMIT: usize = 100;

fn private_helper() {}

mod inner {
    pub fn nested() {}
}
