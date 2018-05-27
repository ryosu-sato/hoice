//! Handles instance splitting.
//!
//! Used to reason separately on each positive/negative clause.

use common::* ;
use unsat_core::UnsatRes ;




/// Splits the instance if asked to do so, and solves it.
///
/// Returns
///
/// - a partial model if the instance is sat
/// - `None` if not in `infer` mode
/// - an error of `Unsat` if unsat
///
/// Assumes the instance is **already pre-processed**.
pub fn work(
  real_instance: Arc<Instance>, _profiler: & Profiler
) -> Res< Option< Either<ConjCandidates, UnsatRes> > > {
  let mut model = ConjCandidates::new() ;

  macro_rules! model {
    (add $model:expr) => ({
      for (pred, tterms) in $model {
        if ! real_instance.is_known(pred) {
          let conj = model.entry(pred).or_insert_with(
            || vec![]
          ) ;
          match tterms.bool() {
            Some(true) => continue,
            Some(false) => conj.clear(),
            None => (),
          }

          if ! conj.iter().any(
            |tts| tts == & tterms || tts.bool() == Some(false)
          ) {
            conj.push(tterms)
          }
        }
      }
    }) ;
  }

  let mut splitter = Splitter::new(& real_instance) ;

  'split_loop: while let Some(preproc_res) = {
    if_not_bench! {
      if let Some((clause, handled, total)) = splitter.info() {
        log! { conf.stats || conf.split_step, || @info
          "\n{}{}{}{}{} Splitting on negative clause #{} ({} of {})",
          conf.emph("|"),
          conf.happy("="),
          conf.sad("="),
          conf.happy("="),
          conf.emph("|"),
          clause, handled + 1, total
        }
        if conf.split_step {
          pause("to start sub-preprocessing", _profiler) ;
        }
      }
    }
    splitter.next_instance(& _profiler)
  } ? {
    if let Some(prof) = splitter.profiler() {
      print_stats("sub-preproc", prof)
    }
    profile! { |_profiler| "sub-system(s)" => add 1 }

    let mut instance = match preproc_res {
      Either::Left(instance) => instance,
      Either::Right(None) => unsat!{
        "by preprocessing"
      },
      Either::Right(Some(this_model)) => {
        log_info! { "sat by preproc\n\n" }
        model! { add this_model }

        continue 'split_loop
      },
    } ;

    if ! conf.infer {
      if conf.split_step {
        pause("to continue", _profiler) ;
      } else {
        log_info! { "Skipping learning..." }
      }

      continue 'split_loop

    } else if conf.split_step {
      pause("to start solving", _profiler) ;
    } else {
      log_info! { "Starting learning..." }
    }

    let res = profile!(
      |_profiler| wrap {
        run_teacher(instance.clone(), & model)
      } "solving"
    ) ? ;

    match res {
      Either::Left(candidates) => {
        log_info! { "sat\n\n" }
        let mut this_model = instance.model_of(candidates) ? ;
        // profile! { |_profiler| tick "waiting" }
        // while Arc::strong_count(& instance) != 1 {}
        // profile! { |_profiler| mark "waiting" }
        if let Some(instance) = Arc::get_mut(& mut instance) {
          instance.simplify_pred_defs(& mut this_model) ?
        }
        model!(add this_model) ;
        // let mut model = real_instance.extend_model(model.clone()) ? ;
        // real_instance.write_model(& model, & mut stdout()) ?
      },

      Either::Right(reason) => return Ok( Some( Either::Right(reason) ) ),
    }

  }

  if conf.infer {
    Ok( Some( Either::Left(model) ) )
  } else {
    Ok(None)
  }
}


/// Runs the teacher on an instance.
pub fn run_teacher(
  instance: Arc<Instance>,
  model: & ConjCandidates,
) -> Res< Either<Candidates, UnsatRes> > {
  let teacher_profiler = Profiler::new() ;
  let solve_res = ::teacher::start_class(
    & instance, model, & teacher_profiler
  ) ;
  print_stats("teacher", teacher_profiler) ;
  solve_res
}




/// Creates new instances by splitting positive/negative clauses.
pub struct Splitter {
  /// The instance we're working on.
  instance: Arc<Instance>,
  /// Clauses to look at separately.
  ///
  /// Indices refer to `self.instance`.
  ///
  /// `Right(once)` means that this splitting is inactive, and `next_instance`
  /// will return `self.instance` if `! once` and `None` otherwise.
  clauses: Either<Vec<ClsIdx>, bool>,
  /// Total number of clauses considered.
  clause_count: usize,
  /// Negative clauses for which we already have a solution.
  prev_clauses: ClsSet,
  /// Profiler.
  _profiler: Option<Profiler>,
}
impl Splitter {

  /// Constructor.
  pub fn new(instance: & Arc<Instance>) -> Self {
    let (clauses, clause_count) = if conf.split
    && instance.neg_clauses().len() > 1 {
      // We want the predicates that appear in the most lhs last (since
      // we're popping).
      let mut clauses: Vec<_> = instance.neg_clauses().iter().map(
        |c| (
          * c,
          if conf.preproc.split_sort {
            instance[* c].lhs_preds().iter().fold(
              0, |
                mut sum, (pred, _)
              | {

                for clause in instance.clauses_of(* pred).0 {
                  if instance[* clause].rhs().is_some() {
                    sum += 1
                  }
                }

                for clause in instance.clauses_of(* pred).1 {
                  if instance[* clause].lhs_preds().is_empty() {
                    // Positive clauses are bad.
                    sum = 0 ;
                    break
                  } else {
                    // sum -= ::std::cmp::min(sum, 1)
                  }
                }

                sum
              }
            )
          } else {
            - (* * c as isize)
          }
        )
      ).collect() ;

      clauses.sort_unstable_by(
        |& (c_1, count_1), & (c_2, count_2)| {
          if   instance[c_1].is_strict_neg()
          && ! instance[c_2].is_strict_neg() {
            ::std::cmp::Ordering::Greater
          } else
          if ! instance[c_1].is_strict_neg()
          &&   instance[c_2].is_strict_neg() {
            ::std::cmp::Ordering::Less
          } else
          if   instance[c_1].from_unrolling
          && ! instance[c_2].from_unrolling {
            ::std::cmp::Ordering::Greater
          } else
          if ! instance[c_1].from_unrolling
          &&   instance[c_2].from_unrolling {
            ::std::cmp::Ordering::Less
          } else {
            count_1.cmp(& count_2)
          }
        }
      ) ;

      // if_verb! {
      //   if conf.preproc.split_sort {
      //     log_verb! {
      //       "sorted clauses:"
      //     }
      //     for & (clause, count) in clauses.iter() {
      //       log_verb! { "#{} ({})", clause, count }
      //       log_debug! {
      //         "{}", instance[clause].to_string_info(instance.preds()).unwrap()
      //       }
      //     }
      //   }
      // }

      let clauses: Vec<_> = clauses.into_iter()
      .map(|(c,_)| c
      // .filter_map(
      //   |(c,_)| if instance[c].from_unrolling {
      //     Some(c)
      //   } else {
      //     Some(c)
      //   }
      ).collect() ;

      let len = clauses.len() ;
      if len <= 1 {
        (Either::Right(false), len)
      } else {
        (Either::Left(clauses), len)
      }
    } else {
      (Either::Right(false), 1)
    } ;

    let instance = instance.clone() ;
    // let model = Vec::new() ;
    Splitter {
      instance, clauses, clause_count,
      prev_clauses: ClsSet::new(), _profiler: None,
    }
  }

  /// Retrieves the profiler.
  pub fn profiler(& mut self) -> Option<Profiler> {
    let mut res = None ;
    ::std::mem::swap(& mut res, & mut self._profiler) ;
    res
  }

  /// Returns the next clause to split on, the number of clauses already
  /// treated and the total number of clauses to handle if active.
  pub fn info(& self) -> Option<(ClsIdx, usize, usize)> {
    match self.clauses {
      Either::Left(ref clauses) => {
        if let Some(clause) = clauses.last() {
          let total = self.clause_count ;
          let count = total - clauses.len() ;
          Some((* clause, count, total))
        } else {
          None
        }
      },
      Either::Right(_) => None,
    }
  }

  /// Returns the next instance to work on.
  pub fn next_instance(& mut self, _prof: & Profiler) -> Res<
    Option< Either<Arc<Instance>, Option<Model>> >
  > {
    match self.clauses {
      Either::Left(ref mut clauses) => if let Some(clause) = clauses.pop() {
        let profiler = Profiler::new() ;
        let preproc_res = profile! (
          |_prof| wrap {
            preproc(
              self.instance.as_ref(), clause, & self.prev_clauses, & profiler
            )
          } "sub-preproc"
        ) ? ;
        self.prev_clauses.insert(clause) ;
        self._profiler = Some(profiler) ;
        Ok(
          Some(
            preproc_res.map_left(
              |sub_instance| Arc::new(sub_instance)
            )
          )
        )
      } else {
        Ok(None)
      },
      Either::Right(ref mut once) => if * once {
        Ok(None)
      } else {
        * once = true ;
        Ok( Some( Either::Left(self.instance.clone()) ) )
      }
    }
  }

}



/// Applies pre-processing to a modified instance.
///
/// Generates the instance obtained by removing all positive (if `pos`,
/// negative otherwise) clauses but `clause`. Preprocesses it and returns the
/// result.
///
/// Fails in debug if the clause is not negative.
fn preproc(
  instance: & Instance, clause: ClsIdx,
  prev_clauses: & ClsSet, profiler: & Profiler
) -> Res< Either<Instance, Option<Model>>> {

  debug_assert! {
    instance[clause].rhs().is_none()
  }

  let instance = ::instance::preproc::work_on_split(
    instance, clause, prev_clauses, profiler
  ) ? ;

  if let Some(maybe_model) = instance.is_trivial_model() ? {
    Ok( Either::Right(maybe_model) )
  } else {
    Ok( Either::Left(instance) )
  }
}