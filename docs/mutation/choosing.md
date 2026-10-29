# Choosing what to mutate

[Mutation testing](README.md) › Choosing what to mutate

Mutants are made per function, never for a whole project at once. Without `--run`, `dgf mutate` only chooses the functions and lists their mutants, which takes a second.

```powershell
dgf mutate                                 # every function in the repository
dgf mutate src/Orders --min-complexity 4   # only functions with complexity 4 or more
dgf mutate --changes                       # only functions this branch changed (vs main)
dgf mutate --changes --show                # list every mutant
dgf mutate --format agent                  # compact, for AI assistants
```

```
MUTANTS  COMPLEXITY  WHY       FUNCTION          LOCATION
     15           4  selected  Pricing.Shipping  src/Shop/Pricing.cs:19-30
                               21:13  weight <= 0  ->  false   (condition)
                               21:13  weight <= 0  ->  !(weight <= 0)   (negate)
                               21:20  <=  ->  >   (relational)
                               21:20  <=  ->  <   (relational)
                               23:13  throw new ArgumentOutOfRangeException(nameof(weight));  ->  ;   (statement, ignored: a guard clause; callers validate weight first (dgf-disable, line 23))
                               25:19  >=  ->  <   (relational)
                               26:9  { return 0; }  ->  {return default;}   (block)
                               29:16  weight > 10  ->  true   (conditional)
                               ...
```

| Option | Chooses |
| --- | --- |
| `--changes` | the functions this branch changed (against `main`, or `--base`), and the functions whose tests it changed |
| `--changes --scope lines` | only the mutants on lines the branch changed |
| `PATHS` | the functions in these files or folders |
| `--min-complexity N` | functions with a cyclomatic complexity of N or more |
| `--neighbours` | with `--changes`, also the lines of other functions that hold a string key a changed function holds (such as `"MemberPlanId"`); listed apart, not in the score |

Not mutated at all:

- functions marked `// dgf-ignore`;
- test code: `*.test.*`, `*.spec.*`, `*Tests.cs`, `*Test.cs`, and anything in a `test`, `tests`, `__tests__`, `spec` or `*.Tests` folder;
- places that only take constants: `const`, attributes, parameter defaults (C#), enum members, `case` and pattern constants; in TypeScript, types, imports, decorators, enums and `x as T`.

Which mutants each function gets: [mutators](mutators.md).
