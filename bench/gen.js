// Deterministic synthetic-module generator. Every section exercises the
// full 0.4.0 construct set (enum + impl/trait, union, newtype, constrict,
// expression-if, match with block arms, T[+]) with index-suffixed names,
// so `sections` scales module size without changing its shape. No
// randomness: identical input → identical output, so numbers stay
// comparable across runs and machines.

function section(i) {
  return `
export interface Display${i}<Self> {
  fmt(self: Self): string;
}

export enum Shape${i} {
  Circle { radius: number },
  Rect { mut w: number, h: number },
}

impl Display${i} for Shape${i} {
  fmt(self): string {
    return match (self) {
      Circle { radius } => \`circle r=\${radius}\`,
      Rect { w, h } => \`rect \${w}x\${h}\`,
    };
  }
}

union Status${i} = 'todo' | 'doing' | 'done';

newtype Id${i} = string;

constrict Id${i} != string;
constrict Id${i} extends string;

export function toStatus${i}(raw: string): Status${i} {
  return Status${i}.has(raw) ? raw : 'todo';
}

export function grade${i}(n: number): string {
  return if (n > 90) { "A" } else if (n > 80) { "B" } else { "C" };
}

export function area${i}(s: Shape${i}): number {
  return match (s) {
    Circle { radius } => {
      const r2 = radius * radius;
      3.14159 * r2
    },
    Rect { w, h } => w * h,
  };
}

export function first${i}(xs: number[+]): number {
  return xs[0];
}

export const sample${i}: string = Shape${i}.fmt(Shape${i}.Rect(3, 4));
`;
}

/** A single large module with `sections` construct groups (~55 lines each). */
export function generateModule(sections) {
  let out = "// @generated synthetic bench module — bench/gen.js\n";
  for (let i = 0; i < sections; i++) out += section(i);
  return out;
}

/** `count` medium modules for the zts-check project corpus. */
export function generateProject(count, sectionsPerFile) {
  const files = {};
  for (let f = 0; f < count; f++) {
    let out = `// @generated synthetic bench module ${f} — bench/gen.js\n`;
    for (let i = 0; i < sectionsPerFile; i++) out += section(`${f}_${i}`);
    files[`mod_${f}.zts`] = out;
  }
  return files;
}
