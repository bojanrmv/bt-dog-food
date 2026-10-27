//! Building the sandbox until every mutant left in it compiles.
//!
//! A compiler error on a line with one mutant removes that mutant. An error
//! on a line with several mutants makes them suspects, and an error on a line
//! without mutants (C# "use of unassigned local variable" points at the use,
//! not at the `&&` that caused it) makes every mutant of that function one:
//! they come out together and go back half at a time until the culprit is
//! found, so the innocent ones are still tested. Such errors stay inside
//! their function, so every function's suspects are tried in the same build.
//! After the first build, only the projects with mutants are rebuilt until
//! they compile; one last build of everything the tests need follows.

use std::collections::{HashMap, HashSet};

use dog_food_core::Mutant;

use super::sandbox::{self, FileMutants, Kind};
use super::{progress, Budget, BuildError, Engine, Outcome, Prepared, Round};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Built {
    Done,
    OutOfTime,
}

/// Most builds before giving up on finding the culprits: every round
/// removes a mutant or halves a group of suspects, so this only guards
/// against a runaway.
const BUILD_ROUNDS: usize = 40;

/// Mutants taken out of the files: (index of their file, mutant).
type Held = Vec<(usize, Mutant)>;

struct Rebuild {
    round: Round,
    /// Groups of suspects still to try, each from one function.
    waiting: Vec<Held>,
    /// The groups of suspects in the files for this build: one per function.
    trying: Vec<Held>,
}

pub fn build(
    prepared: &mut Prepared,
    kind: Kind,
    outcomes: &mut HashMap<usize, Outcome>,
    budget: Budget,
) -> Result<Built, String> {
    let mut state = Rebuild {
        round: Round::All,
        waiting: Vec::new(),
        trying: Vec::new(),
    };
    for _ in 0..BUILD_ROUNDS {
        match prepared.engine.build(state.round, budget) {
            Ok(()) if state.passed(&mut prepared.files) => return Ok(Built::Done),
            Ok(()) => {}
            Err(BuildError::OutOfTime) => return Ok(Built::OutOfTime),
            Err(BuildError::Errors(errors)) => state.failed(
                prepared.engine.as_ref(),
                &mut prepared.files,
                &errors,
                outcomes,
            )?,
        }
        sandbox::write(kind, &prepared.files, &prepared.sandbox.dir)?;
    }
    give_up(prepared, kind, outcomes, budget, state)
}

impl Rebuild {
    /// The build succeeded: the groups being tried are innocent. Returns
    /// whether everything is built.
    fn passed(&mut self, files: &mut [FileMutants]) -> bool {
        self.trying.clear();
        let next = self.next_groups(files);
        if !next.is_empty() {
            let count: usize = next.iter().map(Vec::len).sum();
            progress(&format!(
                "trying {count} suspected mutants again, in {} functions",
                next.len()
            ));
            for group in &next {
                put_back(files, group);
            }
            self.trying = next;
            self.round = Round::Mutated;
            return false;
        }
        if self.round == Round::Mutated {
            progress("the mutated projects compile; building the tests against them");
            self.round = Round::All;
            return false;
        }
        true
    }

    /// One waiting group per function.
    fn next_groups(&mut self, files: &[FileMutants]) -> Vec<Held> {
        let mut functions = HashSet::new();
        let (next, rest): (Vec<Held>, Vec<Held>) = std::mem::take(&mut self.waiting)
            .into_iter()
            .partition(|group| functions.insert(function_of(files, group)));
        self.waiting = rest;
        next
    }

    fn failed(
        &mut self,
        engine: &dyn Engine,
        files: &mut [FileMutants],
        errors: &[String],
        outcomes: &mut HashMap<usize, Outcome>,
    ) -> Result<(), String> {
        let located = engine.locate(files, errors);
        if located.is_empty() {
            return Err(build_error(errors));
        }
        self.round = Round::Mutated;
        if !located.broken.is_empty() {
            progress(&format!(
                "{} mutants do not compile; building again without them",
                located.broken.len()
            ));
            for id in &located.broken {
                outcomes.insert(*id, Outcome::CompileError);
            }
            take_out(files, &located.broken);
            for group in &mut self.trying {
                group.retain(|(_, m)| !located.broken.contains(&m.id));
            }
        }
        let tried = self.blame(files, &located.suspects, outcomes);
        self.suspect(files, &located.suspects, &tried, outcomes);
        // The next groups go in with this rebuild, not after a clean one.
        self.trying = self.next_groups(files);
        for group in &self.trying {
            put_back(files, group);
        }
        Ok(())
    }

    /// The groups being tried that hold a suspect broke the build: they come
    /// out and are halved. Returns the functions that had a group in.
    fn blame(
        &mut self,
        files: &mut [FileMutants],
        suspects: &HashSet<usize>,
        outcomes: &mut HashMap<usize, Outcome>,
    ) -> HashSet<(usize, usize)> {
        let mut tried = HashSet::new();
        for group in std::mem::take(&mut self.trying) {
            tried.insert(function_of(files, &group));
            if group.iter().any(|(_, m)| suspects.contains(&m.id)) {
                take_out(files, &group.iter().map(|(_, m)| m.id).collect());
                self.split(group, outcomes);
            }
        }
        tried
    }

    /// Suspects in functions that had no group in come out, a group per
    /// function, halved at once: the whole group just broke the build. (In a
    /// function whose group was in, the group takes the blame.)
    fn suspect(
        &mut self,
        files: &mut [FileMutants],
        suspects: &HashSet<usize>,
        tried: &HashSet<(usize, usize)>,
        outcomes: &mut HashMap<usize, Outcome>,
    ) {
        let fresh: HashSet<usize> = files
            .iter()
            .enumerate()
            .flat_map(|(index, file)| file.mutants.iter().map(move |m| (index, m)))
            .filter(|(index, m)| {
                suspects.contains(&m.id) && !tried.contains(&function_at(files, *index, m.line))
            })
            .map(|(_, m)| m.id)
            .collect();
        if fresh.is_empty() {
            return;
        }
        progress(&format!(
            "{} mutants are suspected of breaking the build; finding which",
            fresh.len()
        ));
        let held = take_out(files, &fresh);
        for group in by_function(files, held) {
            self.split(group, outcomes);
        }
    }

    /// A group that broke the build: one mutant is the culprit, more are halved.
    fn split(&mut self, mut group: Held, outcomes: &mut HashMap<usize, Outcome>) {
        if group.len() == 1 {
            outcomes.insert(group[0].1.id, Outcome::CompileError);
            return;
        }
        let half = group.split_off(group.len() / 2);
        self.waiting.push(half);
        self.waiting.push(group);
    }
}

/// The function (file index, first line) a group of suspects is in.
fn function_of(files: &[FileMutants], group: &Held) -> (usize, usize) {
    group
        .first()
        .map_or((0, 0), |(index, m)| function_at(files, *index, m.line))
}

/// The innermost chosen function around `line` of file `index`.
fn function_at(files: &[FileMutants], index: usize, line: usize) -> (usize, usize) {
    let start = files[index]
        .functions
        .iter()
        .filter(|(start, end)| (*start..=*end).contains(&line))
        .min_by_key(|(start, end)| end - start)
        .map_or(0, |(start, _)| *start);
    (index, start)
}

fn by_function(files: &[FileMutants], held: Held) -> Vec<Held> {
    let mut groups: Vec<((usize, usize), Held)> = Vec::new();
    for (index, mutant) in held {
        let key = function_at(files, index, mutant.line);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, group)) => group.push((index, mutant)),
            None => groups.push((key, vec![(index, mutant)])),
        }
    }
    groups.into_iter().map(|(_, group)| group).collect()
}

/// Out of rounds: every suspect still unresolved counts as not compiling.
fn give_up(
    prepared: &mut Prepared,
    kind: Kind,
    outcomes: &mut HashMap<usize, Outcome>,
    budget: Budget,
    state: Rebuild,
) -> Result<Built, String> {
    let unresolved: HashSet<usize> = state
        .waiting
        .iter()
        .chain(&state.trying)
        .flatten()
        .map(|(_, m)| m.id)
        .collect();
    eprintln!(
        "note: {} suspected mutants were not isolated and count as not compiling",
        unresolved.len()
    );
    for id in &unresolved {
        outcomes.insert(*id, Outcome::CompileError);
    }
    take_out(&mut prepared.files, &unresolved);
    sandbox::write(kind, &prepared.files, &prepared.sandbox.dir)?;
    match prepared.engine.build(Round::All, budget) {
        Ok(()) => Ok(Built::Done),
        Err(BuildError::OutOfTime) => Ok(Built::OutOfTime),
        Err(BuildError::Errors(errors)) => Err(build_error(&errors)),
    }
}

/// Removes the mutants `ids` from the files and returns them.
fn take_out(files: &mut [FileMutants], ids: &HashSet<usize>) -> Held {
    let mut held = Vec::new();
    for (index, file) in files.iter_mut().enumerate() {
        let (out, kept): (Vec<Mutant>, Vec<Mutant>) = std::mem::take(&mut file.mutants)
            .into_iter()
            .partition(|m| ids.contains(&m.id));
        file.mutants = kept;
        held.extend(out.into_iter().map(|m| (index, m)));
    }
    held
}

fn put_back(files: &mut [FileMutants], held: &Held) {
    for (index, mutant) in held {
        files[*index].mutants.push(mutant.clone());
    }
}

/// The first distinct compiler errors, enough to see what went wrong.
fn build_error(errors: &[String]) -> String {
    let mut seen = HashSet::new();
    let distinct: Vec<&String> = errors.iter().filter(|e| seen.insert(*e)).collect();
    let shown: Vec<&str> = distinct.iter().take(10).map(|e| e.as_str()).collect();
    let more = distinct.len().saturating_sub(shown.len());
    let tail = if more > 0 {
        format!("\n... and {more} more")
    } else {
        String::new()
    };
    format!("the sandbox does not build:\n{}{tail}", shown.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutation::exec::Located;
    use crate::mutation::{Pack, PackRun, Reach, SoloRun};
    use std::path::PathBuf;
    use std::time::Duration;

    /// Reads every compiler error as one that names no mutant.
    struct Unplaced;

    impl Engine for Unplaced {
        fn build(&self, _: Round, _: Budget) -> Result<(), BuildError> {
            unreachable!()
        }
        fn locate(&self, files: &[FileMutants], _: &[String]) -> Located {
            Located {
                broken: HashSet::new(),
                suspects: files
                    .iter()
                    .flat_map(|f| f.mutants.iter().map(|m| m.id))
                    .collect(),
            }
        }
        fn reach(&self, _: Budget) -> Result<Option<Reach>, String> {
            unreachable!()
        }
        fn pack(&self, _: &Pack, _: &Reach, _: Duration, _: &str) -> Result<PackRun, String> {
            unreachable!()
        }
        fn solo(&self, _: usize, _: &Reach, _: Duration, _: &str) -> Result<SoloRun, String> {
            unreachable!()
        }
        fn test_projects(&self) -> Vec<String> {
            Vec::new()
        }
    }

    /// Blames every mutant in a function that holds one of `culprits`, as
    /// an error on a line without mutants does.
    struct Culprits(HashSet<usize>);

    impl Engine for Culprits {
        fn build(&self, _: Round, _: Budget) -> Result<(), BuildError> {
            unreachable!()
        }
        fn locate(&self, files: &[FileMutants], _: &[String]) -> Located {
            let mut suspects = HashSet::new();
            for (index, file) in files.iter().enumerate() {
                for m in file.mutants.iter().filter(|m| self.0.contains(&m.id)) {
                    let function = function_at(files, index, m.line);
                    suspects.extend(
                        file.mutants
                            .iter()
                            .filter(|o| function_at(files, index, o.line) == function)
                            .map(|o| o.id),
                    );
                }
            }
            Located {
                broken: HashSet::new(),
                suspects,
            }
        }
        fn reach(&self, _: Budget) -> Result<Option<Reach>, String> {
            unreachable!()
        }
        fn pack(&self, _: &Pack, _: &Reach, _: Duration, _: &str) -> Result<PackRun, String> {
            unreachable!()
        }
        fn solo(&self, _: usize, _: &Reach, _: Duration, _: &str) -> Result<SoloRun, String> {
            unreachable!()
        }
        fn test_projects(&self) -> Vec<String> {
            Vec::new()
        }
    }

    #[test]
    fn suspects_of_several_functions_are_bisected_in_the_same_builds() {
        let source = "class C {\n  bool F(int a, int b) {\n    return a > 1 && b < 2 || a == b;\n  }\n  bool G(int a, int b) {\n    return a >= 3 && b <= 4 || a != b;\n  }\n}\n";
        let mutants =
            dog_food_core::mutate::mutants(source, dog_food_core::Language::CSharp).unwrap();
        let all: HashSet<usize> = mutants.iter().map(|m| m.id).collect();
        let first = mutants.iter().find(|m| m.line == 3).unwrap().id;
        let second = mutants.iter().rfind(|m| m.line == 6).unwrap().id;
        let engine = Culprits(HashSet::from([first, second]));
        let mut files = vec![FileMutants {
            relative: PathBuf::from("C.cs"),
            source: source.into(),
            mutants,
            functions: vec![(2, 4), (5, 7)],
        }];
        let mut outcomes = HashMap::new();
        let mut state = Rebuild {
            round: Round::All,
            waiting: Vec::new(),
            trying: Vec::new(),
        };
        let mut builds = 0;
        let done = (0..BUILD_ROUNDS).any(|_| {
            builds += 1;
            let placed = files[0].mutants.iter().any(|m| engine.0.contains(&m.id));
            if placed {
                state
                    .failed(
                        &engine,
                        &mut files,
                        &["C.cs(1,1): error CS0165".into()],
                        &mut outcomes,
                    )
                    .unwrap();
                return false;
            }
            state.passed(&mut files)
        });
        assert!(done);
        assert_eq!(
            outcomes,
            HashMap::from([
                (first, Outcome::CompileError),
                (second, Outcome::CompileError)
            ])
        );
        let kept: HashSet<usize> = files[0].mutants.iter().map(|m| m.id).collect();
        assert_eq!(kept, &all - &HashSet::from([first, second]));
        // About one function's halvings, not the two added up.
        let per_function = all.len() / 2;
        assert!(
            builds <= 2 * (usize::BITS - per_function.leading_zeros()) as usize + 4,
            "{builds} builds"
        );
    }

    #[test]
    fn only_the_culprit_of_an_error_without_a_mutant_is_removed() {
        let source =
            "class C {\n  bool F(int a, int b) {\n    return a > 1 && b < 2 || a == b;\n  }\n}\n";
        let mutants =
            dog_food_core::mutate::mutants(source, dog_food_core::Language::CSharp).unwrap();
        let all: HashSet<usize> = mutants.iter().map(|m| m.id).collect();
        assert!(all.len() >= 4);
        let mut files = vec![FileMutants {
            relative: PathBuf::from("F.cs"),
            source: source.into(),
            mutants,
            functions: vec![(2, 4)],
        }];
        // The build fails, with no mutant on the error's line, while mutant 2 is in.
        let mut outcomes = HashMap::new();
        let mut state = Rebuild {
            round: Round::All,
            waiting: Vec::new(),
            trying: Vec::new(),
        };
        let mut rounds = Vec::new();
        let done = (0..BUILD_ROUNDS).any(|_| {
            rounds.push(state.round);
            if files[0].mutants.iter().any(|m| m.id == 2) {
                let errors = ["F.cs(9,1): error CS0165".to_string()];
                state
                    .failed(&Unplaced, &mut files, &errors, &mut outcomes)
                    .unwrap();
                return false;
            }
            state.passed(&mut files)
        });
        assert!(done);
        assert_eq!(outcomes, HashMap::from([(2, Outcome::CompileError)]));
        let kept: HashSet<usize> = files[0].mutants.iter().map(|m| m.id).collect();
        assert_eq!(kept, &all - &HashSet::from([2]));
        // Everything after the first build only rebuilds the mutated projects,
        // except the last build.
        assert_eq!(rounds.first(), Some(&Round::All));
        assert_eq!(rounds.last(), Some(&Round::All));
        assert!(rounds[1..rounds.len() - 1]
            .iter()
            .all(|r| *r == Round::Mutated));
    }
}
