//! Building the sandbox until every mutant left in it compiles.
//!
//! A compiler error on a line with one mutant removes that mutant. An error
//! on a line with several mutants makes them suspects, and an error on a line
//! without mutants (C# "use of unassigned local variable" points at the use,
//! not at the `&&` that caused it) makes every mutant of that function one:
//! they come out together and go back half at a time until the culprit is
//! found, so the innocent ones are still tested. A half that goes back
//! without an error clears itself and convicts the other half, which is
//! halved again without a build of its own. Such errors stay inside
//! their function, so every function's suspects are tried in the same build.
//! While a function has a mutant that does not compile on an error's own
//! line, its other errors may be that mutant's too: nobody else is suspected
//! until it is out.
//! After the first build, only the projects with mutants are rebuilt until
//! they compile; one last build of everything the tests need follows. Where
//! the language has compile checks in memory (C#), the search runs on them
//! after the first build, and builds only confirm what it found.

use std::collections::{HashMap, HashSet};

use dog_food_core::Mutant;

use super::exec::Located;
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
/// Most checks in memory: each takes a fraction of a second.
const CHECK_ROUNDS: usize = 200;

/// Mutants taken out of the files: (index of their file, mutant).
type Held = Vec<(usize, Mutant)>;

struct Rebuild {
    round: Round,
    /// Groups of suspects still to try, each from one function.
    waiting: Vec<Held>,
    /// The groups of suspects in the files for this build: one per function.
    trying: Vec<Held>,
    /// Whether the compile checks in memory ran (after the first failed build).
    checked: bool,
    /// Whether test projects that do not build with no mutant in may be left out.
    may_drop: bool,
    /// Whether every mutant came out once for an error that names none.
    took_all_out: bool,
    /// The search for the mutants behind an error that names none, once
    /// taking them all out and putting them back by function brought it back.
    hunt: Option<Hunt>,
    /// The two halves of a group that broke the build, each by its first
    /// mutant, while both wait untouched.
    halves: HashMap<usize, usize>,
}

impl Rebuild {
    fn new(may_drop: bool) -> Rebuild {
        Rebuild {
            round: Round::All,
            waiting: Vec::new(),
            trying: Vec::new(),
            checked: false,
            may_drop,
            took_all_out: false,
            hunt: None,
            halves: HashMap::new(),
        }
    }
}

/// An error that names no mutant (a type that changes in one function and
/// fails in another, or in a test project) and comes back when the mutants
/// go back by function. Every build puts half of the groups still out back
/// in: a clean build clears that half, and an error narrows the search to
/// it, so each culprit takes a few builds and every build makes progress.
#[derive(Default)]
struct Hunt {
    /// Out, not tried yet.
    candidates: Vec<Held>,
    /// Out, set aside while the search narrows to the half being tried.
    later: Vec<Held>,
    /// Back in for this build.
    trying: Vec<Held>,
}

impl Hunt {
    /// Puts half of the candidates back in; false when none is left.
    fn next(&mut self, files: &mut [FileMutants]) -> bool {
        if self.candidates.is_empty() {
            self.candidates = std::mem::take(&mut self.later);
        }
        if self.candidates.is_empty() {
            return false;
        }
        let rest = self.candidates.split_off(self.candidates.len().div_ceil(2));
        self.trying = std::mem::replace(&mut self.candidates, rest);
        for group in &self.trying {
            put_back(files, group);
        }
        true
    }

    /// The groups tried broke the build: the search narrows to them. A
    /// single mutant is the culprit; a single group is halved.
    fn narrow(&mut self, files: &mut [FileMutants], outcomes: &mut HashMap<usize, Outcome>) {
        let tried = std::mem::take(&mut self.trying);
        for group in &tried {
            take_out(files, &group.iter().map(|(_, m)| m.id).collect());
        }
        self.later.append(&mut self.candidates);
        self.candidates = match <[Held; 1]>::try_from(tried) {
            Ok([group]) if group.len() == 1 => {
                outcomes.insert(group[0].1.id, Outcome::CompileError);
                Vec::new()
            }
            Ok([mut group]) => {
                let half = group.split_off(group.len() / 2);
                vec![group, half]
            }
            Err(tried) => tried,
        };
    }

    fn mutants(&self) -> usize {
        self.candidates
            .iter()
            .chain(&self.later)
            .chain(&self.trying)
            .map(Vec::len)
            .sum()
    }
}

/// `may_drop`: test projects that do not build even with no mutant in the
/// sandbox are left out, and the others still run. `left_out` gets the
/// mutants the search for an error that names none did not reach before the
/// builds ran out: they are not tested, and not judged.
pub fn build(
    prepared: &mut Prepared,
    kind: Kind,
    outcomes: &mut HashMap<usize, Outcome>,
    budget: Budget,
    may_drop: bool,
    left_out: &mut HashSet<usize>,
) -> Result<Built, String> {
    let mut state = Rebuild::new(may_drop);
    for _ in 0..BUILD_ROUNDS {
        match prepared.engine.build(state.round, budget) {
            Ok(()) if state.passed(&mut prepared.files, outcomes) => return Ok(Built::Done),
            Ok(()) => {}
            Err(BuildError::OutOfTime) => return Ok(Built::OutOfTime),
            Err(BuildError::Errors(errors)) => {
                build_failed(prepared, kind, outcomes, &mut state, &errors, budget)?;
            }
        }
        sandbox::write(kind, &prepared.files, &prepared.sandbox.dir)?;
    }
    give_up(prepared, kind, outcomes, budget, state, left_out)
}

/// A failed build: with no mutant in, test projects that do not build are
/// left out when they may be; otherwise the errors decide which mutants
/// come out, and the first failure starts the compile checks in memory.
fn build_failed(
    prepared: &mut Prepared,
    kind: Kind,
    outcomes: &mut HashMap<usize, Outcome>,
    state: &mut Rebuild,
    errors: &[String],
    budget: Budget,
) -> Result<(), String> {
    if state.may_leave_out(&prepared.files) && prepared.engine.drop_broken(errors, budget) {
        // The next build is of everything, without them.
        return Ok(());
    }
    state.failed(
        prepared.engine.as_ref(),
        &mut prepared.files,
        errors,
        outcomes,
    )?;
    if !std::mem::replace(&mut state.checked, true) {
        check_in_memory(prepared, kind, outcomes, state)?;
    }
    Ok(())
}

/// Goes on with the search on compile checks in memory, after a build failed:
/// each round writes the files and checks them in a fraction of a second.
/// When nothing is left to try, the next build is of everything the tests
/// need. Without checks, or when they fail, the builds go on as before.
fn check_in_memory(
    prepared: &mut Prepared,
    kind: Kind,
    outcomes: &mut HashMap<usize, Outcome>,
    state: &mut Rebuild,
) -> Result<(), String> {
    let Some(mut checker) = prepared.engine.checker(&prepared.files) else {
        return Ok(());
    };
    let started = std::time::Instant::now();
    for round in 1..=CHECK_ROUNDS {
        sandbox::write(kind, &prepared.files, &prepared.sandbox.dir)?;
        let errors = match checker.errors() {
            Ok(errors) => errors,
            Err(err) => {
                debug!("build", "compile checks in memory stopped: {err}");
                return Ok(());
            }
        };
        if errors.is_empty() && state.clean_in_memory(&mut prepared.files, outcomes) {
            debug!(
                "build",
                "compile checks in memory found every mutant that does not compile: {round} rounds, {:.1} s",
                started.elapsed().as_secs_f64()
            );
            return Ok(());
        }
        if !errors.is_empty() {
            debug!(
                "build",
                "check {round}: {} errors, first: {}",
                errors.len(),
                errors
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
            state.failed(
                prepared.engine.as_ref(),
                &mut prepared.files,
                &errors,
                outcomes,
            )?;
        }
    }
    Ok(())
}

impl Rebuild {
    /// Whether a failed build may leave test projects out: one of everything,
    /// with no mutant in, when that is allowed.
    fn may_leave_out(&self, files: &[FileMutants]) -> bool {
        self.may_drop && self.round == Round::All && no_mutants(files)
    }

    /// A check in memory found no error: the groups being tried are innocent,
    /// and the next go in. Returns whether none is left, and the next build is
    /// of everything the tests need.
    fn clean_in_memory(
        &mut self,
        files: &mut [FileMutants],
        outcomes: &mut HashMap<usize, Outcome>,
    ) -> bool {
        if self.hunt_goes_on(files) {
            return false;
        }
        self.clear_tried(outcomes);
        let next = self.next_groups(files);
        if next.is_empty() {
            self.round = Round::All;
            return true;
        }
        for group in &next {
            put_back(files, group);
        }
        self.trying = next;
        false
    }

    /// The build succeeded: the groups being tried are innocent. Returns
    /// whether everything is built.
    fn passed(
        &mut self,
        files: &mut [FileMutants],
        outcomes: &mut HashMap<usize, Outcome>,
    ) -> bool {
        if self.hunt_goes_on(files) {
            return false;
        }
        self.clear_tried(outcomes);
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

    /// When the builds run out: the suspects of errors on their lines or in
    /// their functions, and the mutants a hunt had not cleared yet.
    fn unresolved(&self) -> (HashSet<usize>, HashSet<usize>) {
        let ids = |groups: &[Held]| -> HashSet<usize> {
            groups.iter().flatten().map(|(_, m)| m.id).collect()
        };
        let suspects = &ids(&self.waiting) | &ids(&self.trying);
        let hunted = self.hunt.as_ref().map_or_else(HashSet::new, |h| {
            &(&ids(&h.candidates) | &ids(&h.later)) | &ids(&h.trying)
        });
        (suspects, hunted)
    }

    /// A clean build or check while hunting: the groups tried stay in, and
    /// the next half goes in. False once the hunt is over (or none runs).
    fn hunt_goes_on(&mut self, files: &mut [FileMutants]) -> bool {
        let Some(hunt) = self.hunt.as_mut() else {
            return false;
        };
        hunt.trying.clear();
        if hunt.next(files) {
            self.round = Round::All;
            return true;
        }
        self.hunt = None;
        false
    }

    /// Whether the hunt takes this failed build: one is under way, or an
    /// error that names no mutant came back after they all came out once.
    fn hunted(
        &mut self,
        files: &mut [FileMutants],
        located: &Located,
        outcomes: &mut HashMap<usize, Outcome>,
    ) -> bool {
        if self.hunt.is_some() && !located.broken.is_empty() {
            // Those mutants explain the build: the same half goes again without them.
            self.remove_broken(files, &located.broken, outcomes);
            return true;
        }
        if self.hunt.is_none() && !self.came_back(files, located) {
            return false;
        }
        self.hunt_failed(files, outcomes);
        true
    }

    /// An error that names no mutant, with mutants in, after they all came
    /// out once and the build without them passed.
    fn came_back(&self, files: &[FileMutants], located: &Located) -> bool {
        located.is_empty() && self.took_all_out && !no_mutants(files)
    }

    /// The error that names no mutant came back: the hunt starts with every
    /// mutant out, or narrows to the groups it just put back.
    fn hunt_failed(&mut self, files: &mut [FileMutants], outcomes: &mut HashMap<usize, Outcome>) {
        self.round = Round::All;
        if let Some(hunt) = self.hunt.as_mut() {
            hunt.narrow(files, outcomes);
            if !hunt.next(files) {
                self.hunt = None;
            }
            return;
        }
        let all: HashSet<usize> = files
            .iter()
            .flat_map(|f| f.mutants.iter().map(|m| m.id))
            .collect();
        let held = take_out(files, &all);
        let mut candidates = by_function(files, held);
        candidates.append(&mut self.waiting);
        self.trying.clear();
        let mut hunt = Hunt {
            candidates,
            ..Hunt::default()
        };
        progress(&format!(
            "an error that names no mutant came back; looking for its cause among {} mutants, half of what is left per build",
            hunt.mutants()
        ));
        if hunt.next(files) {
            self.hunt = Some(hunt);
        }
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
        if self.hunted(files, &located, outcomes) {
            return Ok(());
        }
        if located.is_empty() {
            if self.searching() {
                self.search_on(files);
                return Ok(());
            }
            return self.take_all_out(files, errors);
        }
        self.round = Round::Mutated;
        // Where a mutant on an error's own line does not compile, the
        // function's other errors may be its doing: they wait a build.
        let explained = functions_of(files, &located.broken);
        self.remove_broken(files, &located.broken, outcomes);
        let mut tried = self.blame(files, &located.suspects, &explained, outcomes);
        tried.extend(explained);
        self.suspect(files, &located.suspects, &tried, outcomes);
        // The next groups go in with this rebuild, not after a clean one.
        self.trying = self.next_groups(files);
        for group in &self.trying {
            put_back(files, group);
        }
        Ok(())
    }

    /// Whether suspects are still being tried function by function, before
    /// every mutant came out once.
    fn searching(&self) -> bool {
        !self.took_all_out && !(self.waiting.is_empty() && self.trying.is_empty())
    }

    /// An error that names no mutant while suspects are still being tried:
    /// their search goes on first, every function side by side, and the
    /// error waits. Taking every mutant out now would send the suspects
    /// through the search for that error, one culprit at a time. The groups
    /// that were in stay in, neither blamed nor cleared (the error may be
    /// theirs), and say nothing about their other halves.
    fn search_on(&mut self, files: &mut [FileMutants]) {
        for group in std::mem::take(&mut self.trying) {
            if let Some((_, first)) = group.first() {
                self.forget_half(first.id);
            }
        }
        self.trying = self.next_groups(files);
        for group in &self.trying {
            put_back(files, group);
        }
    }

    /// Mutants on an error's line do not compile: they come out for good.
    fn remove_broken(
        &mut self,
        files: &mut [FileMutants],
        broken: &HashSet<usize>,
        outcomes: &mut HashMap<usize, Outcome>,
    ) {
        if broken.is_empty() {
            return;
        }
        progress(&format!(
            "{} mutants do not compile; building again without them",
            broken.len()
        ));
        for id in broken {
            outcomes.insert(*id, Outcome::CompileError);
        }
        take_out(files, broken);
        // A group that loses a mutant no longer says anything about its other half.
        let lost: Vec<usize> = self
            .trying
            .iter()
            .filter(|group| group.iter().any(|(_, m)| broken.contains(&m.id)))
            .filter_map(|group| group.first().map(|(_, m)| m.id))
            .collect();
        for first in lost {
            self.forget_half(first);
        }
        let groups = self
            .trying
            .iter_mut()
            .chain(self.hunt.iter_mut().flat_map(|h| h.trying.iter_mut()));
        for group in groups {
            group.retain(|(_, m)| !broken.contains(&m.id));
        }
    }

    /// The groups in for a build that found nothing wrong in their functions
    /// are innocent, and stay in.
    fn clear_tried(&mut self, outcomes: &mut HashMap<usize, Outcome>) {
        for group in std::mem::take(&mut self.trying) {
            self.cleared(&group, outcomes);
        }
    }

    /// `group` went back in without an error: the two halves broke the build
    /// together, so the culprit is in the other one, which is halved again
    /// at once. A single mutant left that way still goes in alone, for a
    /// build to show it: when the cause is outside the function after all,
    /// nobody in it is to blame.
    fn cleared(&mut self, group: &Held, outcomes: &mut HashMap<usize, Outcome>) {
        let Some(first) = group.first().map(|(_, m)| m.id) else {
            return;
        };
        let Some(other) = self.forget_half(first) else {
            return;
        };
        let waits = |g: &Held| g.first().is_some_and(|(_, m)| m.id == other);
        if let Some(at) = self.waiting.iter().position(waits) {
            if self.waiting[at].len() > 1 {
                let guilty = self.waiting.remove(at);
                self.split(guilty, outcomes);
            }
        }
    }

    /// Forgets that the group starting with `first` has another half; that
    /// half's first mutant, if it had one.
    fn forget_half(&mut self, first: usize) -> Option<usize> {
        let other = self.halves.remove(&first)?;
        self.halves.remove(&other);
        Some(other)
    }

    /// An error nothing ties to a mutant: every mutant comes out, once, and
    /// everything the tests need is built. If the sandbox then builds, a mutant
    /// caused it and they go back by function, to find which. If it does not,
    /// the code does not compile without mutants (or a test project does not:
    /// the build leaves it out when it may).
    fn take_all_out(&mut self, files: &mut [FileMutants], errors: &[String]) -> Result<(), String> {
        let all: HashSet<usize> = files
            .iter()
            .flat_map(|f| f.mutants.iter().map(|m| m.id))
            .collect();
        if all.is_empty() {
            return Err(build_error(errors));
        }
        progress(&format!(
            "a compiler error names none of the {} mutants in the build; taking them all out to see whether they caused it",
            all.len()
        ));
        let held = take_out(files, &all);
        self.waiting.extend(by_function(files, held));
        self.trying.clear();
        self.took_all_out = true;
        // Not only the mutated projects: a test project may be what fails.
        self.round = Round::All;
        Ok(())
    }

    /// The groups being tried that hold a suspect broke the build: they come
    /// out and are halved. In an `explained` function (one with a mutant
    /// that does not compile on an error's own line) a group comes out and
    /// waits as it is: that mutant may be all that was wrong. The other
    /// groups are innocent. Returns the functions that had a group in.
    fn blame(
        &mut self,
        files: &mut [FileMutants],
        suspects: &HashSet<usize>,
        explained: &HashSet<(usize, usize)>,
        outcomes: &mut HashMap<usize, Outcome>,
    ) -> HashSet<(usize, usize)> {
        let mut tried = HashSet::new();
        for group in std::mem::take(&mut self.trying) {
            let function = function_of(files, &group);
            tried.insert(function);
            if !group.iter().any(|(_, m)| suspects.contains(&m.id)) {
                self.cleared(&group, outcomes);
                continue;
            }
            take_out(files, &group.iter().map(|(_, m)| m.id).collect());
            if let Some((_, first)) = group.first() {
                self.forget_half(first.id);
            }
            if explained.contains(&function) {
                self.waiting.push(group);
            } else {
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

    /// A group that broke the build: one mutant is the culprit, more are
    /// halved, and the halves remember each other (see [`Rebuild::cleared`]).
    fn split(&mut self, mut group: Held, outcomes: &mut HashMap<usize, Outcome>) {
        if group.len() == 1 {
            outcomes.insert(group[0].1.id, Outcome::CompileError);
            return;
        }
        let half = group.split_off(group.len() / 2);
        self.halves.insert(half[0].1.id, group[0].1.id);
        self.halves.insert(group[0].1.id, half[0].1.id);
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

/// The outermost chosen function around `line` of file `index`, with the
/// lambdas in it: a mutant outside a lambda can break the code inside it, so
/// they count as one, and an innocent lambda is not blamed on its own.
fn function_at(files: &[FileMutants], index: usize, line: usize) -> (usize, usize) {
    let start = files[index]
        .functions
        .iter()
        .filter(|(start, end)| (*start..=*end).contains(&line))
        .max_by_key(|(start, end)| end - start)
        .map_or(0, |(start, _)| *start);
    (index, start)
}

/// The functions the mutants `ids`, still in the files, are in.
fn functions_of(files: &[FileMutants], ids: &HashSet<usize>) -> HashSet<(usize, usize)> {
    files
        .iter()
        .enumerate()
        .flat_map(|(index, file)| file.mutants.iter().map(move |m| (index, m)))
        .filter(|(_, m)| ids.contains(&m.id))
        .map(|(index, m)| function_at(files, index, m.line))
        .collect()
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
/// Out of rounds: every suspect still unresolved (an error was on its line,
/// or on its function's) counts as not compiling. The mutants a hunt had
/// not cleared were never shown to break the build: they are left out,
/// untested.
fn give_up(
    prepared: &mut Prepared,
    kind: Kind,
    outcomes: &mut HashMap<usize, Outcome>,
    budget: Budget,
    state: Rebuild,
    left_out: &mut HashSet<usize>,
) -> Result<Built, String> {
    let (unresolved, hunted) = state.unresolved();
    if !unresolved.is_empty() {
        tellln!(
            "note: {} suspected mutants were not isolated and count as not compiling",
            unresolved.len()
        );
    }
    for id in &unresolved {
        outcomes.insert(*id, Outcome::CompileError);
    }
    take_out(&mut prepared.files, &unresolved);
    if !hunted.is_empty() {
        tellln!(
            "note: {} mutants were left out untested: an error that names no mutant came back, and the builds ran out before the search for its cause reached them",
            hunted.len()
        );
        take_out(&mut prepared.files, &hunted);
        left_out.extend(hunted);
    }
    sandbox::write(kind, &prepared.files, &prepared.sandbox.dir)?;
    match prepared.engine.build(Round::All, budget) {
        Ok(()) => Ok(Built::Done),
        Err(BuildError::OutOfTime) => Ok(Built::OutOfTime),
        Err(BuildError::Errors(errors)) => Err(build_error(&errors)),
    }
}

fn no_mutants(files: &[FileMutants]) -> bool {
    files.iter().all(|f| f.mutants.is_empty())
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
        fn solo(
            &self,
            _: usize,
            _: &Reach,
            _: bool,
            _: Duration,
            _: &str,
        ) -> Result<SoloRun, String> {
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
        fn solo(
            &self,
            _: usize,
            _: &Reach,
            _: bool,
            _: Duration,
            _: &str,
        ) -> Result<SoloRun, String> {
            unreachable!()
        }
        fn test_projects(&self) -> Vec<String> {
            Vec::new()
        }
    }

    /// Ties no compiler error to any mutant.
    struct Nowhere;

    impl Engine for Nowhere {
        fn build(&self, _: Round, _: Budget) -> Result<(), BuildError> {
            unreachable!()
        }
        fn locate(&self, _: &[FileMutants], _: &[String]) -> Located {
            Located::default()
        }
        fn reach(&self, _: Budget) -> Result<Option<Reach>, String> {
            unreachable!()
        }
        fn pack(&self, _: &Pack, _: &Reach, _: Duration, _: &str) -> Result<PackRun, String> {
            unreachable!()
        }
        fn solo(
            &self,
            _: usize,
            _: &Reach,
            _: bool,
            _: Duration,
            _: &str,
        ) -> Result<SoloRun, String> {
            unreachable!()
        }
        fn test_projects(&self) -> Vec<String> {
            Vec::new()
        }
    }

    #[test]
    fn an_error_tied_to_no_mutant_takes_them_all_out_before_giving_up() {
        let source = "class C {\n  bool F(int a) {\n    return a > 1;\n  }\n}\n";
        let mutants =
            dog_food_core::mutate::mutants(source, dog_food_core::Language::CSharp).unwrap();
        let count = mutants.len();
        assert!(count > 0);
        let mut files = vec![FileMutants {
            relative: PathBuf::from("C.cs"),
            source: source.into(),
            mutants,
            functions: vec![(2, 4)],
        }];
        let mut state = Rebuild::new(true);
        let mut outcomes = HashMap::new();
        let errors = vec!["Other.cs(1,1): error CS1503: nothing to do with a mutant".to_string()];
        state
            .failed(&Nowhere, &mut files, &errors, &mut outcomes)
            .expect("the mutants may have caused it");
        assert!(files[0].mutants.is_empty(), "all out for one clean build");
        assert!(no_mutants(&files));
        assert_eq!(state.round, Round::All, "of everything, test projects too");
        assert!(state.may_leave_out(&files));
        assert!(!Rebuild::new(false).may_leave_out(&files));
        assert!(outcomes.is_empty(), "nobody is blamed yet");
        assert_eq!(state.waiting.iter().map(Vec::len).sum::<usize>(), count);
        // Still failing with none in: the code itself does not compile.
        let again = state.failed(&Nowhere, &mut files, &errors, &mut outcomes);
        assert!(again.unwrap_err().contains("does not build"));
        // Or it builds, and they go back to find which.
        assert!(!state.passed(&mut files, &mut outcomes));
        assert_eq!(files[0].mutants.len(), count);
    }

    /// Builds until done, failing with an error that names no mutant while
    /// any of `culprits` is in; the builds it took.
    fn hunt_down(culprits: &HashSet<usize>) -> (usize, Vec<FileMutants>, HashMap<usize, Outcome>) {
        let source = "class C {\n  bool F(int a, int b) {\n    return a > 1 && b < 2 || a == b;\n  }\n  bool G(int a, int b) {\n    return a >= 3 && b <= 4 || a != b;\n  }\n  bool H(int a) {\n    return a < 5 && a > 0;\n  }\n}\n";
        let mutants =
            dog_food_core::mutate::mutants(source, dog_food_core::Language::CSharp).unwrap();
        let mut files = vec![FileMutants {
            relative: PathBuf::from("C.cs"),
            source: source.into(),
            mutants,
            functions: vec![(2, 4), (5, 7), (8, 10)],
        }];
        let mut outcomes = HashMap::new();
        let mut state = Rebuild::new(true);
        let errors = ["Tests/CTests.cs(9,1): error CS0029: a type changed elsewhere".to_string()];
        for builds in 1..=BUILD_ROUNDS {
            let broken = files[0].mutants.iter().any(|m| culprits.contains(&m.id));
            if !broken && state.passed(&mut files, &mut outcomes) {
                return (builds, files, outcomes);
            }
            if broken {
                state
                    .failed(&Nowhere, &mut files, &errors, &mut outcomes)
                    .unwrap();
            }
        }
        panic!("not found in {BUILD_ROUNDS} builds: {outcomes:?}");
    }

    #[test]
    fn an_error_that_names_no_mutant_and_comes_back_is_hunted_down() {
        let all: Vec<usize> = dog_food_core::mutate::mutants(
            "class C {\n  bool F(int a, int b) {\n    return a > 1 && b < 2 || a == b;\n  }\n  bool G(int a, int b) {\n    return a >= 3 && b <= 4 || a != b;\n  }\n  bool H(int a) {\n    return a < 5 && a > 0;\n  }\n}\n",
            dog_food_core::Language::CSharp,
        )
        .unwrap()
        .iter()
        .map(|m| m.id)
        .collect();
        assert!(all.len() > 12, "{}", all.len());
        // One culprit, in the middle function; then one in each of two functions.
        for culprits in [
            HashSet::from([all[all.len() / 2]]),
            HashSet::from([all[1], all[all.len() - 2]]),
        ] {
            let (builds, files, outcomes) = hunt_down(&culprits);
            let blamed: HashSet<usize> = outcomes.keys().copied().collect();
            assert_eq!(blamed, culprits, "only the culprits are blamed");
            assert!(outcomes.values().all(|o| *o == Outcome::CompileError));
            let kept: HashSet<usize> = files[0].mutants.iter().map(|m| m.id).collect();
            let expected: HashSet<usize> = all
                .iter()
                .copied()
                .filter(|id| !culprits.contains(id))
                .collect();
            assert_eq!(kept, expected, "every innocent mutant is back in");
            assert!(builds <= 12 * culprits.len() + 6, "{builds} builds");
        }
    }

    #[test]
    fn a_hunt_cut_short_leaves_its_mutants_untested_not_broken() {
        let source = "class C {\n  bool F(int a, int b) {\n    return a > 1 && b < 2 || a == b;\n  }\n  bool G(int a, int b) {\n    return a >= 3 && b <= 4 || a != b;\n  }\n}\n";
        let mutants =
            dog_food_core::mutate::mutants(source, dog_food_core::Language::CSharp).unwrap();
        let all: HashSet<usize> = mutants.iter().map(|m| m.id).collect();
        let mut files = vec![FileMutants {
            relative: PathBuf::from("C.cs"),
            source: source.into(),
            mutants,
            functions: vec![(2, 4), (5, 7)],
        }];
        let mut outcomes = HashMap::new();
        let mut state = Rebuild::new(true);
        let errors = ["Tests/CTests.cs(9,1): error CS0029: elsewhere".to_string()];
        // Taken out, back in by function, back again: the hunt starts.
        state
            .failed(&Nowhere, &mut files, &errors, &mut outcomes)
            .unwrap();
        assert!(!state.passed(&mut files, &mut outcomes));
        state
            .failed(&Nowhere, &mut files, &errors, &mut outcomes)
            .unwrap();
        assert!(state.hunt.is_some());
        let (suspects, hunted) = state.unresolved();
        assert!(suspects.is_empty(), "{suspects:?}");
        let in_files: HashSet<usize> = files[0].mutants.iter().map(|m| m.id).collect();
        assert_eq!(
            &hunted | &in_files,
            all,
            "every mutant is in, or the hunt holds it"
        );
        assert!(outcomes.is_empty(), "nobody is blamed");
    }

    #[test]
    fn an_error_that_names_no_mutant_waits_for_the_search_under_way() {
        let source = "class C {\n  bool F(int a, int b) {\n    return a > 1 && b < 2 || a == b;\n  }\n  bool G(int a, int b) {\n    return a >= 3 && b <= 4 || a != b;\n  }\n}\n";
        let mutants =
            dog_food_core::mutate::mutants(source, dog_food_core::Language::CSharp).unwrap();
        let culprit = mutants.iter().find(|m| m.line == 3).unwrap().id;
        let mut files = vec![FileMutants {
            relative: PathBuf::from("C.cs"),
            source: source.into(),
            mutants,
            functions: vec![(2, 4), (5, 7)],
        }];
        let mut outcomes = HashMap::new();
        let mut state = Rebuild::new(true);
        let errors = ["C.cs(1,1): error CS0165".to_string()];
        // F's mutants are suspects, and half of them wait.
        state
            .failed(
                &Culprits(HashSet::from([culprit])),
                &mut files,
                &errors,
                &mut outcomes,
            )
            .unwrap();
        assert!(state.searching());
        let waiting: usize = state.waiting.iter().map(Vec::len).sum();
        assert!(waiting > 0);
        // An error elsewhere, with no mutant to its name: nobody comes out
        // for it yet, and the next suspects go in.
        let in_before = files[0].mutants.len();
        state
            .failed(&Nowhere, &mut files, &errors, &mut outcomes)
            .unwrap();
        assert!(!state.took_all_out);
        assert!(files[0].mutants.len() > in_before, "the search went on");
        assert!(outcomes.is_empty());
        // With nothing left to try, it takes them all out.
        while state.searching() {
            state
                .failed(&Nowhere, &mut files, &errors, &mut outcomes)
                .unwrap();
        }
        state
            .failed(&Nowhere, &mut files, &errors, &mut outcomes)
            .unwrap();
        assert!(state.took_all_out && no_mutants(&files));
        assert!(
            outcomes.is_empty(),
            "an error nobody was seen to cause blames nobody"
        );
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
        let mut state = Rebuild::new(true);
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
            state.passed(&mut files, &mut outcomes)
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
        // About one function's halvings, not the two added up, and one build
        // per halving: a half that goes back clean convicts the other.
        let per_function = all.len() / 2;
        assert!(
            builds <= (usize::BITS - per_function.leading_zeros()) as usize + 5,
            "{builds} builds"
        );
    }

    /// Blames the mutant on an error's own line, and with it every other
    /// mutant of its function, as a second error on a line without mutants does.
    struct OnItsLine(usize);

    impl Engine for OnItsLine {
        fn build(&self, _: Round, _: Budget) -> Result<(), BuildError> {
            unreachable!()
        }
        fn locate(&self, files: &[FileMutants], _: &[String]) -> Located {
            let others = files[0].mutants.iter().map(|m| m.id);
            Located {
                broken: HashSet::from([self.0]),
                suspects: others.filter(|id| *id != self.0).collect(),
            }
        }
        fn reach(&self, _: Budget) -> Result<Option<Reach>, String> {
            unreachable!()
        }
        fn pack(&self, _: &Pack, _: &Reach, _: Duration, _: &str) -> Result<PackRun, String> {
            unreachable!()
        }
        fn solo(
            &self,
            _: usize,
            _: &Reach,
            _: bool,
            _: Duration,
            _: &str,
        ) -> Result<SoloRun, String> {
            unreachable!()
        }
        fn test_projects(&self) -> Vec<String> {
            Vec::new()
        }
    }

    #[test]
    fn a_mutant_that_does_not_compile_explains_its_functions_other_errors() {
        let source =
            "class C {\n  bool F(int a, int b) {\n    return a > 1 && b < 2 || a == b;\n  }\n}\n";
        let mutants =
            dog_food_core::mutate::mutants(source, dog_food_core::Language::CSharp).unwrap();
        let all: HashSet<usize> = mutants.iter().map(|m| m.id).collect();
        let culprit = mutants[1].id;
        let mut files = vec![FileMutants {
            relative: PathBuf::from("F.cs"),
            source: source.into(),
            mutants,
            functions: vec![(2, 4)],
        }];
        let mut outcomes = HashMap::new();
        let mut state = Rebuild::new(true);
        let errors = ["F.cs(3,1): error CS0019".to_string()];
        state
            .failed(&OnItsLine(culprit), &mut files, &errors, &mut outcomes)
            .unwrap();
        // The others stay in for the next build, which shows they compile.
        let kept: HashSet<usize> = files[0].mutants.iter().map(|m| m.id).collect();
        assert_eq!(kept, &all - &HashSet::from([culprit]));
        assert!(state.waiting.is_empty() && state.trying.is_empty());
        assert!(
            !state.passed(&mut files, &mut outcomes),
            "the tests' build is next"
        );
        assert!(state.passed(&mut files, &mut outcomes));
        assert_eq!(outcomes, HashMap::from([(culprit, Outcome::CompileError)]));
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
        let mut state = Rebuild::new(true);
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
            state.passed(&mut files, &mut outcomes)
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
