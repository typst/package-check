use typst::{eval::Tracer, model::Document};

use crate::world::SystemWorld;

use super::{convert_diagnostics, Diagnostics};

pub fn check(diags: &mut Diagnostics, world: &SystemWorld) -> Option<Document> {
    let mut tracer = Tracer::new();
    let result = typst::compile(world, &mut tracer);
    diags
        .warnings
        .extend(convert_diagnostics(&world, tracer.warnings()));

    match result {
        Ok(doc) => Some(doc),
        Err(errors) => {
            diags.errors.extend(convert_diagnostics(&world, errors));
            None
        }
    }
}
