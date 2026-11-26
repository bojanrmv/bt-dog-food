# Changelog

What each version changed for people who use `dgf`. A version is not released without a section here: CI and the release workflow check it, and the section becomes the notes of the GitHub release ([releasing](docs/releasing.md)). Links inside a section are full URLs, because the release page shows it too.

## 0.1.7

New:

- `dgf outline <files, folders or globs>` prints every type and member of a file with its lines and signature, without the bodies. `--changes` takes the files the branch changed, `--match` and `--public` narrow the list.
- `dgf show FILE:Name`, `FILE:Type.Member`, `FILE:LINE` or `FILE:FROM-TO` prints that code with line numbers. A bare name is looked for under `--in`; `--signature` prints only the first line and the comment above it.
- `dgf refs <names> --in <dir>` lists every line that uses a name, with the declaration it is in: declarations first, tests last. It goes by the name in the syntax tree, so comments and strings are not matched.
- `dgf coverage <reports>` prints line and branch coverage per file, the lines that never ran and the lines that ran with only some of their branches. `--changes` covers only what the branch changed, `--floor 90` exits with code 1 below 90%, `--functions` adds a row per function.
- `dgf skill install --workflow` adds four stage skills (`dgf-spec`, `dgf-build`, `dgf-clean`, `dgf-harden`) that take a piece of work from a request to hardened tests. `dgf skill show <name>` prints one.

The four lookup commands read the source files as they are, with no build and no index. Each takes many targets in one call and says what it left out.

Fixed:

- `dgf mutate` on TypeScript: a mutant of a chained `a ?? b ?? c` no longer stops the build. JavaScript does not allow `??` beside `&&` or `||` without parentheses, so the mutant now has them.
