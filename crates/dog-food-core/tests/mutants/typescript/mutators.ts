// settings: level=standard ignore_methods=console.log
function save(_: unknown): void {}

// mutant: block {... -> {}
export function arithmetic(a: number, b: number): number {
  // mutant: arithmetic + -> -
  // mutant: arithmetic * -> /
  return a + b * 2;
}

// mutant: block {... -> {}
export function compare(a: number, s: string): boolean {
  // mutant: relational < -> <=
  // mutant: relational < -> >=
  // mutant: equality === -> !==
  // mutant: logical && -> ||
  // mutant: condition a < 1 -> true
  // mutant: condition s === "x" -> true
  // mutant: condition a < 1 && s === "x" -> true
  // mutant: condition a < 1 && s === "x" -> false
  // mutant: string "x" -> ""
  return a < 1 && s === "x";
}

// mutant: block {... -> {}
export function branches(a: number, items: number[]): number {
  // mutant: condition a > 1 -> true
  // mutant: condition a > 1 -> false
  // mutant: relational > -> >=
  // mutant: relational > -> <=
  // mutant: block { return -a; } -> {}
  // mutant: unary - -> +
  if (a > 1) { return -a; }
  // mutant: condition a -> false
  // mutant: block { a--; } -> {}
  // mutant: update -- -> ++
  while (a) { a--; }
  // mutant: condition i < items.length -> false
  // mutant: relational < -> <=
  // mutant: relational < -> >=
  // mutant: update ++ -> --
  // mutant: block { continue; } -> {}
  for (let i = 0; i < items.length; i++) { continue; }
  // mutant: condition items.length === 0 -> true
  // mutant: condition items.length === 0 -> false
  // mutant: equality === -> !==
  // mutant: arithmetic + -> -
  return items.length === 0 ? 0 : a + 1;
}

// mutant: block {... -> {}
export function strings(name: string): string {
  // mutant: string `Hi ${name}` -> ``
  const a = `Hi ${name}`;
  // mutant: string "" -> "dgf was here!"
  const b = "";
  // mutant: object { "key": 1 } -> {}
  const c = { "key": 1 };
  // mutant: method name.trim() -> name
  // mutant: method startsWith -> endsWith
  // mutant: string "x" -> ""
  const d = name.trim().startsWith("x");
  // mutant: array [a, b, String(c)] -> []
  // mutant: condition d -> true
  // mutant: condition d -> false
  // mutant: string "y" -> ""
  // mutant: string "n" -> ""
  return [a, b, String(c)].join(d ? "y" : "n");
}

// mutant: block {... -> {}
export function chains(o?: { b?: number[] }): number | undefined {
  // mutant: arrow o?.b?.length -> undefined
  // mutant: optional ?. -> .
  // mutant: optional ?. -> .
  const f = () => o?.b?.length;
  // mutant: array [1, 2] -> []
  const xs = [1, 2];
  // mutant: array [] -> ["dgf was here"]
  const ys: number[] = [];
  // mutant: array (3) -> ()
  const zs = new Array(3);
  // mutant: method xs.filter((x) => x > 1) -> xs
  // mutant: relational > -> >=
  // mutant: relational > -> <=
  // mutant: condition x > 1 -> true
  // mutant: condition x > 1 -> false
  // mutant: arrow x > 1 -> undefined
  const big = xs.filter((x) => x > 1);
  // mutant: call save(big); -> ;
  save(big);
  // mutant: relational > -> >=
  // mutant: relational > -> <=
  // mutant: condition ys.length > 0 -> true
  // mutant: condition ys.length > 0 -> false
  save(ys.length > 0 ? zs : big);
  return f();
}

// mutant: block {... -> {}
export function assign(a: number, s: string): string {
  let n = a;
  // mutant: assignment += -> -=
  n += 2;
  let m: number | undefined;
  // mutant: assignment ??= -> &&=
  m ??= 1;
  // mutant: string "x" -> ""
  s += "x";
  // mutant: call console.log("n", n > 1) -> ; (ignored)
  // mutant: string "n" -> "" (ignored)
  // mutant: relational > -> >= (ignored)
  // mutant: relational > -> <= (ignored)
  // mutant: condition n > 1 -> true (ignored)
  // mutant: condition n > 1 -> false (ignored)
  console.log("n", n > 1)
  // mutant: arithmetic * -> / (ignored)
  // dgf-disable-next-line arithmetic: unit conversion
  const k = n * 2
  // mutant: arithmetic + -> -
  // mutant: arithmetic + -> -
  return s + (k + m);
}

// mutant: block {... -> {}
export function cases(k: string): number {
  switch (k) {
    // mutant: string "a" -> ""
    case "a":
      // mutant: condition return 1; -> ;
      return 1;
    default:
      // mutant: condition return 0; -> ;
      return 0;
  }
}

class Base {
  constructor(protected x: number) {}
}

export class Child extends Base {
  constructor(x: number) {
    super(x);
  }

  // mutant: block {... -> {}
  // mutant: arithmetic + -> -
  get double(): number { return this.x + this.x; }
}

// The regex mutator.
// mutant: block {... -> {}
export function patterns(s: string): boolean {
  // mutant: regex /^\d+$/ -> /\d+$/
  // mutant: regex /^\d+$/ -> /^\D+$/
  // mutant: regex /^\d+$/ -> /^\d$/
  // mutant: regex /^\d+$/ -> /^\d+/
  const digits = /^\d+$/;
  // An emptied pattern cannot be `//`, which starts a comment.
  // mutant: regex /^/ -> /(?:)/
  const empty = /^/;
  // mutant: regex "a+[\\w]" -> "a+[\\W]"
  // mutant: regex "a+[\\w]" -> "a+[^\\w]"
  // mutant: regex "a+[\\w]" -> "a[\\w]"
  // mutant: string "a+[\\w]" -> ""
  // mutant: string "g" -> ""
  const built = new RegExp("a+[\\w]", "g");
  // mutant: condition digits.test(s) && empty.test(s) && built.test(s) -> false
  // mutant: condition digits.test(s) && empty.test(s) && built.test(s) -> true
  // mutant: condition digits.test(s) && empty.test(s) -> true
  // mutant: logical && -> ||
  // mutant: logical && -> ||
  return digits.test(s) && empty.test(s) && built.test(s);
}

// A template string spanning lines: the copy writes its line break as `\n`.
// mutant: block {... -> {}
export function lines(items: number[]): string[] {
  // mutant: arrow `item ${i}` -> undefined
  // mutant: arrow i > 1 -> undefined
  // mutant: condition i > 1 -> false
  // mutant: condition i > 1 -> true
  // mutant: method items.filter((i) => i > 1) -> items
  // mutant: relational > -> <=
  // mutant: relational > -> >=
  // mutant: string `item ${i}` -> ``
  return items.filter((i) => i > 1).map((i) => `item
${i}`);
}

// A tagged template keeps its line breaks as written: a mutant that would
// copy it onto one line (`filter` removed) is not made.
// mutant: block {... -> {}
export function raw(items: string[]): string[] {
  return items
    // mutant: string `a b` -> ``
    .concat(String.raw`a
b`)
    // mutant: arrow s -> undefined
    .filter((s) => s);
}
