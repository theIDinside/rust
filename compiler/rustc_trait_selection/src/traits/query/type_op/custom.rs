use crate::infer::canonical::query_response;
use crate::infer::InferCtxt;
use crate::traits::query::type_op::TypeOpOutput;
use crate::traits::query::Fallible;
use crate::traits::ObligationCtxt;
use rustc_infer::infer::region_constraints::RegionConstraintData;
use rustc_middle::traits::query::NoSolution;
use rustc_span::source_map::DUMMY_SP;

use std::fmt;

pub struct CustomTypeOp<F> {
    closure: F,
    description: &'static str,
}

impl<F> CustomTypeOp<F> {
    pub fn new<'tcx, R>(closure: F, description: &'static str) -> Self
    where
        F: FnOnce(&ObligationCtxt<'_, 'tcx>) -> Fallible<R>,
    {
        CustomTypeOp { closure, description }
    }
}

impl<'tcx, F, R: fmt::Debug> super::TypeOp<'tcx> for CustomTypeOp<F>
where
    F: FnOnce(&ObligationCtxt<'_, 'tcx>) -> Fallible<R>,
{
    type Output = R;
    /// We can't do any custom error reporting for `CustomTypeOp`, so
    /// we can use `!` to enforce that the implementation never provides it.
    type ErrorInfo = !;

    /// Processes the operation and all resulting obligations,
    /// returning the final result along with any region constraints
    /// (they will be given over to the NLL region solver).
    fn fully_perform(self, infcx: &InferCtxt<'tcx>) -> Fallible<TypeOpOutput<'tcx, Self>> {
        if cfg!(debug_assertions) {
            info!("fully_perform({:?})", self);
        }

        Ok(scrape_region_constraints(infcx, self.closure)?.0)
    }
}

impl<F> fmt::Debug for CustomTypeOp<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.description.fmt(f)
    }
}

/// Executes `op` and then scrapes out all the "old style" region
/// constraints that result, creating query-region-constraints.
pub fn scrape_region_constraints<'tcx, Op: super::TypeOp<'tcx, Output = R>, R>(
    infcx: &InferCtxt<'tcx>,
    op: impl FnOnce(&ObligationCtxt<'_, 'tcx>) -> Fallible<R>,
) -> Fallible<(TypeOpOutput<'tcx, Op>, RegionConstraintData<'tcx>)> {
    // During NLL, we expect that nobody will register region
    // obligations **except** as part of a custom type op (and, at the
    // end of each custom type op, we scrape out the region
    // obligations that resulted). So this vector should be empty on
    // entry.
    let pre_obligations = infcx.take_registered_region_obligations();
    assert!(
        pre_obligations.is_empty(),
        "scrape_region_constraints: incoming region obligations = {:#?}",
        pre_obligations,
    );

    let value = infcx.commit_if_ok(|_| {
        let ocx = ObligationCtxt::new_in_snapshot(infcx);
        let value = op(&ocx)?;
        let errors = ocx.select_all_or_error();
        if errors.is_empty() {
            Ok(value)
        } else {
            infcx.tcx.sess.delay_span_bug(
                DUMMY_SP,
                format!("errors selecting obligation during MIR typeck: {:?}", errors),
            );
            Err(NoSolution)
        }
    })?;

    let region_obligations = infcx.take_registered_region_obligations();
    let region_constraint_data = infcx.take_and_reset_region_constraints();
    let region_constraints = query_response::make_query_region_constraints(
        infcx.tcx,
        region_obligations
            .iter()
            .map(|r_o| (r_o.sup_type, r_o.sub_region, r_o.origin.to_constraint_category()))
            .map(|(ty, r, cc)| (infcx.resolve_vars_if_possible(ty), r, cc)),
        &region_constraint_data,
    );

    if region_constraints.is_empty() {
        Ok((
            TypeOpOutput { output: value, constraints: None, error_info: None },
            region_constraint_data,
        ))
    } else {
        Ok((
            TypeOpOutput {
                output: value,
                constraints: Some(infcx.tcx.arena.alloc(region_constraints)),
                error_info: None,
            },
            region_constraint_data,
        ))
    }
}
