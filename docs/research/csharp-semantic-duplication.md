# Finding C# code that does the same thing: research and plan

Status: research, for review before more code is written. Scope: C#, deterministic methods only (no machine learning), aimed at code that can be refactored into one shared version.

## 1. Summary

- Deciding in general whether two pieces of code do the same thing is impossible (Rice's theorem): no tool can be both complete and always right. What works in practice is a **ladder of evidence**. Each rung recognises more ways of writing the same thing, at a higher cost, and the report says which rung matched.
  1. **Same text** after formatting: copy-paste.
  2. **Same syntax shape** after renaming: what `dgf-dup` finds today.
  3. **Same shape with small edits**: an added, removed or changed statement (near-miss clones).
  4. **Same meaning after normalization**: a `for` loop and a `foreach`, `if/else` and `?:`, `x += 1` and `x = x + 1`, a loop and the LINQ call that computes the same value. This is the "written differently" case.
  5. **Same behaviour on tested inputs**: both run on generated inputs and agree.
  6. **Proven equal**: a solver shows they agree for every input. This is only feasible for small, pure functions.
- For C#, the strongest lever is the compiler, not syntax. Roslyn already turns most of C#'s syntax variety into one form:
  - its **operation tree** (`IOperation`) gives one node kind for every assignment, every call, every conversion, and every comparison, whatever the syntax;
  - its **control-flow graph** (`Microsoft.CodeAnalysis.FlowAnalysis.ControlFlowGraph`) lowers every `for`, `while`, `foreach`, `if`, `switch`, `?:`, `&&`, `||` and `?.` to basic blocks with plain branches.

  Hashing a normalized version of that graph catches rungs 2–4 for real C#, with types and overloads resolved, which tree-sitter cannot do.
- Recommendation:
  - keep `dgf-dup` (Rust, tree-sitter) as the fast, multi-language syntactic tool for rungs 1–3;
  - add a **C# semantic engine** (.NET, Roslyn) for rungs 4–6, which writes the same report format, so one HTML page and one `--changes` gate cover both;
  - build an **evaluation corpus first**, so each phase is measured, not guessed.

## 2. What "doing the same thing" means

The literature names four kinds of clone, and the definitions matter for what we promise:

| Kind | Meaning | Example |
| --- | --- | --- |
| Type 1 | Identical apart from whitespace, layout and comments | copy-paste |
| Type 2 | Type 1, with identifiers, literals and types renamed | `plan.Price` vs `seat.Fee` |
| Type 3 | Type 2, with statements added, removed or changed | a copy with one extra check |
| Type 4 | Same behaviour, different syntax | a `foreach` that sums vs `items.Sum(x => x.Price)` |

For refactoring, "the same" needs a precise meaning: **same observable effects for the same inputs**. That is the value returned, the writes to fields, parameters and captured variables, the calls made to other code (in order, where order matters), and the exceptions thrown. Two consequences:

- A function that also logs is not equal to one that does not, but it may still be "equal apart from one call". That is a Type 3 relation on the normalized form, and it is useful to report.
- Equality can be **conditional**. `list.First()` and `list[0]` agree only when `list` is non-empty, and they throw different exceptions when it is empty. The engine has to either treat them as different or report the condition.

## 3. Techniques, and how they fit C#

### 3.1 Text and tokens

Tools like PMD's CPD and jscpd hash runs of tokens. SourcererCC (Sajnani et al., ICSE 2016) compares bags of tokens through an inverted index and scales to 250 MLOC with about 86% precision and 86–100% recall for Types 1–3. They're fast and simple, and they don't find Type 4 at all. `dgf-dup` already does better than plain token runs.

### 3.2 Syntax trees

- **Characteristic vectors** (DECKARD, Jiang et al., ICSE 2007) count node kinds per subtree and group similar vectors with locality-sensitive hashing. This finds Type 3 at scale without comparing every pair.
- **Anti-unification** computes the most specific template two trees share; the differences become holes. It's what `dgf-dup` does now, and it's what turns a finding into a refactoring plan (the parameters).
- **Limit:** a syntax tree has no types and no meaning. `a.Count` on a list and on a dictionary look the same, and a `for` loop and a `foreach` look different.

### 3.3 The compiler's normalized forms (Roslyn)

This is the part specific to C#, and the biggest step up.

- **The semantic model:** every name is resolved to a symbol, every expression has a type, every call has its chosen overload. Renaming a local is then provably harmless, and two calls to different overloads are provably different.
- **The operation tree (`IOperation`)** removes much syntactic variety:
  - `x += 1`, `x = x + 1` and `x++` become assignment operations with an explicit binary operation underneath;
  - implicit conversions become explicit conversion operations;
  - `a?.b` becomes a conditional-access operation;
  - pattern matching, `is`, `switch` expressions and `switch` statements become a small set of pattern operations.
- **The control-flow graph** (`ControlFlowGraph.Create`) lowers a method to basic blocks. In each, operations run in order with no branch except at the end, and there are at most two outgoing edges: fall-through and conditional.
  - `for`, `while`, `do` and `foreach` all become blocks with a back edge.
  - `if/else`, `?:`, `&&`, `||`, `??` and `switch` all become conditional branches.
  - Lambdas and local functions get their own graphs, reachable from the enclosing one.

  This removes the "same logic, different control syntax" difference almost for free.
- **Data flow:** `SemanticModel.AnalyzeDataFlow` and the CFG give, for any region, the variables flowing in, written inside, read after and captured. That's exactly what extracting a method needs: its parameters and return values. It would replace the heuristics `dgf-dup` uses today.

### 3.4 SSA, value numbering and canonical expressions

On the CFG, rewrite each method into static single assignment form, one name per value, then apply **global value numbering**. That gives equivalent expressions the same number and hash-conses identical computations. Canonical keys include the operation, its operands' value numbers and its type, but not where the result is stored. Commutative operations sort their operands, so `a + b` and `b + a` collide. On top of that, apply cheap compiler clean-ups:
- copy propagation: `var t = x; use(t)` becomes `use(x)`;
- constant folding;
- dead-code removal;
- removing locals that only rename a value.

The result is a canonical value graph for each method, and it hashes the same for most "written differently" variants that don't change the algorithm.

### 3.5 Program dependence graphs (PDG)

A program dependence graph keeps only what each statement really depends on: its data dependences and its control dependences, not the order the statements happen to be written in.
- Komondoor and Horwitz (SAS 2001) find clones as isomorphic PDG subgraphs grown by backward slicing from matching statements. That finds **non-contiguous**, **reordered** and **intertwined** clones, and the clones tend to be meaningful computations, good for extraction. Their follow-up (FASE 2002) extracts such clones into procedures.
- Exact subgraph isomorphism is expensive and misses near-miss matches. **Weisfeiler–Lehman (WL) hashing** relabels each node from its neighbours' labels for a few rounds. It gives a fingerprint that equal graphs always share, so candidates can be bucketed quickly. CCGraph (ASE 2020) uses an approximate WL-based graph matching for PDG clone detection.
- In C#, the CFG plus data flow from 3.3 gives a PDG without writing our own dependence analysis.

### 3.6 Rewriting to normal forms (equality saturation)

Some equivalences aren't just syntax; they're algebra or library knowledge:
- `x * 2`, `x + x` and `x << 1`;
- `!(a && b)` and `!a || !b`;
- `list.Count() > 0` and `list.Any()`;
- `string.Format("{0}", x)` and `$"{x}"`.

**E-graphs** keep all forms of an expression reachable by a set of rewrite rules at once, as equivalence classes. **Equality saturation** applies the rules until nothing new appears (the `egg` library, POPL 2021, reports up to 3000× faster rebuilding than earlier approaches). Two expressions are equal under the rules when they land in the same e-class.
- For us the rules are a curated, tested list of C# and BCL identities, each with its conditions. Some identities don't hold for `NaN` or overflow, so those are restricted to the types where they're true.
- This is precise and explainable: the report can name the rule that matched.

### 3.7 Idioms: loops and LINQ

Most "same thing, different way" code in C# business logic is a loop written by hand next to a LINQ call, or two different loops:

```csharp
decimal total = 0;
foreach (var line in order.Lines)
    if (line.Active) total += line.Price * line.Quantity;

var total = order.Lines.Where(l => l.Active).Sum(l => l.Price * l.Quantity);
```

The plan is to recognise loop idioms on the CFG and SSA form and summarise each loop by what it computes:
- **reduce** (sum, count, min, max, aggregate);
- **any/all**;
- **find-first**;
- **map into a list**;
- **filter into a list**;
- **group into a dictionary**.

The summary records the source, the filter and selector bodies (as canonical value graphs), the initial value and the combining operation. The matching LINQ chains get the same summary. Then both forms above hash to `reduce(+, 0, Lines, filter: l.Active, select: l.Price*l.Quantity)`. This idea is called "loop idiom recognition" in compilers. It's the single most useful Type 4 rule for C#, and it only needs a fixed, well-tested set of patterns.

### 3.8 Behaviour: testing candidates against each other

EqMiner (Jiang and Su, ISSTA 2009) cut code into fragments and ran each on random inputs, clustering fragments that gave the same outputs. It scaled to the Linux kernel with days of parallel processing, but it needs many inputs and produces false matches when inputs don't reach the interesting paths. For us:
- testing is a **confirmation step on candidates** found statically, never the search itself;
- generate inputs from parameter types (property-based testing, as FsCheck does for .NET), run both versions in dgf's existing mutation sandbox, and compare return values and effects;
- a disagreement disproves equality and shows the input that tells them apart, which is useful in itself;
- agreement only raises confidence; the report says "agreed on N inputs", not "equal".

### 3.9 Proof: symbolic execution and SMT

For small pure functions (arithmetic, comparisons, string checks), both versions can be translated to formulas and a solver asked whether any input makes them differ. Z3 has an official .NET binding. Differential symbolic execution and ARDiff (Badihi et al., ESEC/FSE 2020) do this for program versions. ARDiff abstracts the code the two share and refines only when needed, and solved more equivalence cases than earlier method-level tools. Loops and non-linear arithmetic limit how far this scales, so it's an optional top rung for the cases where it works, and it can prove "different" with a counterexample.

### 3.10 Comparing compiled IL

Compiling both and comparing IL (Mono.Cecil, ICSharpCode.Decompiler) gets the compiler's lowering of `async`, iterators, `using`, `lock`, lambdas and pattern matching for free. But the IL carries compiler noise (temporaries, closure classes, optimisation differences) and needs a build. The Roslyn CFG gives most of the same lowering without a build, so IL is a fallback, not the plan.

### 3.11 Not used: learned models

Neural detectors (code2vec, CodeBERT, graph neural networks such as SEED) report strong Type 4 scores on benchmarks. They're excluded here by design: they can't say why two pieces of code match, and their scores have proven fragile across benchmarks. On SemanticCloneBench, results of models that scored well on BigCloneBench moved by more than 20%.

## 4. Measuring: a corpus before code

Public benchmarks are a poor fit:
- BigCloneBench is Java. A 2025 study found about 93% of 406 sampled "Weak Type-3/Type-4" pairs did not actually share functionality, so it can't measure semantic detection.
- SemanticCloneBench has a C# set of pairs from Stack Overflow answers, which makes it useful for recall, but its pairs are small and self-contained.

So the plan starts with our own C# corpus, built the way mutation-based clone benchmarks are:

1. **Equivalent pairs by transformation:** take real methods and apply transformations that keep the meaning. Each gets a test that runs both versions on the same inputs, so the pair is guaranteed equal:
   - `for` ↔ `foreach` ↔ `while`;
   - loop ↔ LINQ;
   - `if/else` ↔ `?:` ↔ `switch` expression;
   - early return ↔ nested `if`;
   - invert a condition and swap the branches;
   - extract or inline a local;
   - reorder independent statements;
   - string concatenation ↔ interpolation ↔ `StringBuilder`;
   - `x == null` ↔ `x is null`;
   - De Morgan.
2. **Near-miss pairs that must not match:**
   - `<` vs `<=`, `First` vs `FirstOrDefault`, `Any` vs `All`, a dropped null check, a swapped argument order;
   - the same shape on different types (`decimal` vs `double` rounding).
3. **Real clones** from open-source C# repositories, labelled by hand, to measure precision on real code.

Every phase reports precision, recall and run time on this corpus. A phase is done when it meets its targets.

## 5. Review of `dgf-dup` as it stands

What works:
- It's fast: about 2,000 C# files in 1.6 s. The design of seeds, pairwise growth, classes and templates is sound.
- Its findings are refactoring plans: a template, holes with kinds, unite advice, cautions, and lines saved. It handles repeated statements in a row, whole-function copies, and wrapper and overlap removal.
- It has `--changes` with a merge-base diff and a gate, an agent format, JSON and an HTML page.

What's weak, and how a semantic engine fixes it:

| Weakness | Effect | Fix |
| --- | --- | --- |
| No types: every name becomes a placeholder | False matches: same shape, different meaning (`list.Count` vs `dict.Count`, different overloads) | Roslyn symbols: holes carry types; different overloads are different |
| Contiguous statements only, in order | Misses reordered and interleaved copies, and copies with one statement inserted | Type 3 gaps (phase 1), PDG slicing (phase 5) |
| Syntax only | Misses every Type 4 case (`for` vs `foreach`, loop vs LINQ, `if` vs `?:`) | CFG + SSA canonical forms (phase 3), idioms and rules (phase 4) |
| Declarations and later uses found by token heuristics | Wrong parameter and return advice in some cases (typed declarations, `out var`, pattern variables, deconstruction) | Roslyn data-flow analysis: exact inputs, outputs and captured variables |
| Hole kinds guessed from neighbouring tokens | `member` vs `call` vs `type` can be wrong | Symbols give the exact kind |
| Statement granularity | Misses copied expressions inside larger statements (a long LINQ chain, a big object initializer) | Expression-level candidates on the operation tree |
| Ranking by lines saved only | Boilerplate can outrank risky logic | Combine with complexity and CRAP (phase 7) |
| `MAX_SEEDS = 64` caps pairwise growth | A pattern repeated more often loses copies silently | Group by exact fingerprint first, compare only representatives |
| Only one level of evidence | The report can't say how sure it is | Every finding carries its rung (Section 1) |

`dgf-dup` stays useful as it is for TypeScript and Rust, and as the quick first pass for C#.

## 6. Proposed architecture

```
           ┌───────────────────────────┐        ┌──────────────────────────────────┐
 sources ─►│ dgf-dup (Rust, tree-sitter)│        │ dgf-dup-cs (.NET, Roslyn)         │
           │  rungs 1-3, all languages │        │  load solution → compilation      │
           └────────────┬──────────────┘        │  per method: IOperation, CFG      │
                        │                       │   → SSA + value numbering         │
                        │                       │   → idiom summaries, rewrite rules│
                        │                       │   → fingerprints (several levels) │
                        │                       │  candidates: buckets, WL hashes   │
                        │                       │  verify: canonical anti-unify     │
                        │                       │  confirm: tests in sandbox, Z3    │
                        │                       └────────────────┬─────────────────┘
                        ▼                                        ▼
                 one report format (JSON) ──► merge, rank (with CRAP) ──► table · agent · HTML · --changes gate
```

- **Engine:** a .NET tool, `dgf-dup-cs`, using Roslyn (`Microsoft.CodeAnalysis.CSharp`) and `MSBuildWorkspace` with MSBuildLocator. The workspace loads the solution with real references, so symbols and types are exact. No build is needed. A restore is, which dgf's mutation sandbox already handles.
- **Output:** the same clone-class JSON `dgf-dup` writes, with extra fields: the rung, the rule or idiom that matched, exact parameters and return values from data flow, and the confirmation result. One HTML page and one `--changes` gate cover both tools.
- **Fingerprints, several per method or fragment:**
  - syntax shape, which matches `dgf-dup`;
  - normalized operation tree;
  - canonical CFG + value graph;
  - idiom summary;
  - a WL hash of the dependence graph.

  Exact fingerprints bucket candidates in linear time, and LSH on the vectors finds near-misses.
- **Verification:** every candidate pair is checked on its canonical form with anti-unification. That yields the template, typed holes and the extraction plan, so nothing is reported from a hash collision alone.
- **Confirmation (optional, opt-in):** generated tests run in dgf's sandbox for candidates at rung 4. Z3 runs for small pure functions.

## 7. Plan in phases

Each phase ends with code, tests on the corpus, and documentation: a user guide section, a design note for the technique, and examples in the report.

| Phase | What | Done when |
| --- | --- | --- |
| 0. Corpus and harness | The C# corpus from Section 4 (transformation-generated equivalent pairs with tests, must-not-match pairs, labelled real clones); a harness that scores any tool's JSON for precision, recall and time | `dgf-dup` today is measured; the numbers are the baseline |
| 1. Syntactic tool, reviewed | Fix the table in Section 5 where no semantics are needed: Type 3 near-misses with gaps, expression-level candidates, no `MAX_SEEDS` loss, exact-fingerprint grouping, the evidence level in the report | Type 3 recall up without precision loss on the corpus; still about 2 s on 2,000 files |
| 2. Roslyn engine skeleton | `dgf-dup-cs` loads a solution, walks methods, hashes the normalized operation tree, and writes the shared JSON. Types and overloads go into holes, and data flow gives exact parameters and returns | Type 2 precision up (no same-shape-different-type matches); exact extraction plans |
| 3. Control flow and values | CFG → SSA → value numbering, with copy propagation, constant folding, dead code and commutative order; canonical graph hash | Equal for `for`/`foreach`/`while`, `if`/`?:`/`switch`, early return vs nested, extracted vs inlined locals |
| 4. Idioms and rules | Loop idiom summaries (reduce, any/all, find, map, filter, group) matched with LINQ chains; a curated, tested rule set in an e-graph (algebra, Boolean, null checks, string building, BCL identities with their conditions) | Loop ↔ LINQ pairs found, the rule named in the report; no must-not-match pair matches |
| 5. Dependence graphs | PDG from CFG and data flow; backward slicing from matching statements (Komondoor–Horwitz) for reordered and interleaved clones; WL hashing for candidates | Reordered and interleaved pairs found; the report shows which statements form the clone |
| 6. Confirmation | Generated inputs from parameter types, run in dgf's sandbox, compare returns and effects; Z3 for small pure functions. A counterexample disproves a match | Every reported rung-4 match has a confirmation result; false matches on the corpus shown as disproved |
| 7. One tool | `dgf duplication` and the `dgf changes` gate on both engines; ranking with complexity and CRAP (a copied function that is complex and untested ranks first); HTML shows the evidence level and the rule | A PR gate that reports "this branch adds a copy of X", with how sure it is |

Phases 0–3 are the core: they give exact, typed, control-flow-insensitive detection for C#, which no free tool offers today. Visual Studio's own clone analysis (VS 2012–2019 Enterprise) found renamed and reordered statements but not semantic clones. Phases 4–6 add the "written differently" cases with an explanation for each match.

## 8. Risks and open questions

- **Loading cost:** `MSBuildWorkspace` on a large solution takes time and needs a restore. Mitigation: cache per-project fingerprints by file hashes, as the mutation cache does, and only re-analyse changed projects with `--changes`.
- **Side effects:** equality must include writes and calls. Methods with I/O, time, randomness or database access are compared structurally, never "confirmed" by running them.
- **Exceptions and edge cases:** rules hold only under their conditions (overflow, `NaN`, empty collections, nulls). Every rule needs a test that pins its conditions.
- **Generics, `async`, iterators and `dynamic`:** the CFG handles `async` and iterators at the operation level. `dynamic` is compared structurally only.
- **Noise:** generated code, records, DTO mapping and boilerplate. Ranking by lines saved and complexity, plus excludes, keeps them below real findings.
- **Scope:** "the same thing" will stay partial. The report has to say what kind of sameness it found and how it knows, never just "duplicate".

## 9. Sources

- Ain et al., "A Systematic Review on Code Clone Detection", IEEE Access 2019. https://doaj.org/article/93f3d00c9557499e8a7c4c947d672fbd
- Sajnani et al., "SourcererCC: Scaling Code Clone Detection to Big Code", ICSE 2016. https://arxiv.org/abs/1512.06448
- Jiang, Misherghi, Su, Glondu, "DECKARD: Scalable and Accurate Tree-Based Detection of Code Clones", ICSE 2007. https://www.cs.ucdavis.edu/~su/publications/icse07.pdf
- Jiang, "Scalable Detection of Similar Code: Techniques and Applications", dissertation, UC Davis 2009. https://www.cs.ucdavis.edu/~su/theses/LJ-dissertation.pdf
- Jiang and Su, "Automatic Mining of Functionally Equivalent Code Fragments via Random Testing", ISSTA 2009. https://bibtex.github.io/ISSTA-2009-JiangS.html
- Deng et al., "Challenges of the Dynamic Detection of Functionally Similar Code Fragments" (critique of EqMiner-style detection). https://arxiv.org/pdf/1801.06107
- Komondoor and Horwitz, "Using Slicing to Identify Duplication in Source Code", SAS 2001. https://research.cs.wisc.edu/wpis/abstracts/sas01.abs.html
- Zou et al., "CCGraph: a PDG-based Code Clone Detector With Approximate Graph Matching", ASE 2020. https://yinxingxue.github.io/papers/ase2020_CCGraph%20A%20PDG%20based%20Code%20Clone%20Detector%20With%20Approximate%20Graph%20Matching.pdf
- Li et al., "Detecting Similar Programs via the Weisfeiler-Leman Graph Kernel", ICSR 2016. https://www.aminer.org/pub/57d0640bac4436735429a0b0/detecting-similar-programs-via-the-weisfeiler-leman-graph-kernel
- Willsey et al., "egg: Fast and Extensible Equality Saturation", POPL 2021. https://arxiv.org/abs/2004.03082
- Value numbering and hash-consing: https://bernsteinbear.com/blog/value-numbering/ and "Global Value Numbering: A Precise and Efficient Algorithm". https://arxiv.org/pdf/1504.03239
- Badihi et al., "ARDiff: Scaling Program Equivalence Checking via Iterative Abstraction and Refinement of Common Code", ESEC/FSE 2020. https://people.ece.ubc.ca/~mjulia/publications/ARDiff_2020.pdf
- Roslyn control-flow graph API: https://learn.microsoft.com/en-us/dotnet/api/microsoft.codeanalysis.flowanalysis?view=roslyn-dotnet and source https://source.dot.net/Microsoft.CodeAnalysis/Operations/ControlFlowGraph.cs.html
- Benchmarks: "How the Misuse of a Dataset Harmed Semantic Clone Detection" (BigCloneBench Type 3/4 labels). https://arxiv.org/abs/2505.04311v1 ; GPTCloneBench and SemanticCloneBench overview. https://alphaxiv.org/overview/2308.13963v2
- Xue et al., "SEED: Semantic Graph based Deep detection for type-4 clone" (learned, for contrast). https://deepai.org/publication/seed-semantic-graph-based-deep-detection-for-type-4-clone
- Visual Studio's clone analysis (VS 2012–2019). https://learn.microsoft.com/id-id/previous-versions/visualstudio/visual-studio-2012/hh205279(v=vs.110)
