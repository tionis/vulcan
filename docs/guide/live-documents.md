# Live documents

Notes can compute: Dataview queries and inline expressions, DataviewJS that shares one scope per
note, CustomJS helper classes (see the [scripting guide](scripting.md)), Meta Bind input fields and
views, and Numerals calculator blocks. Vulcan renders all of them as their Obsidian plugins do, in `vulcan render`, `note get
--evaluate`, the editor's virtual text, HTML publication, and the web views. Rendering only reads;
values are written back to notes by explicit commands. The full contract is
[`docs/specs/live-documents.md`](../specs/live-documents.md).

## Meta Bind fields

[Meta Bind](https://github.com/mProjectsCode/obsidian-meta-bind-plugin) fields bind inputs and
computed views to a note's properties. Vulcan reads the same syntax and shows each field with the
value it binds to; Meta Bind fields are on by default (`metabind.enabled`), and
`vulcan config import metabind` imports the plugin's templates, excluded folders, and options.

````markdown
---
hp: 7
max_hp: 10
alive: true
class: rogue
---
Alive `INPUT[toggle:alive]` · HP `INPUT[slider(maxValue(10)):hp]` / `VIEW[{max_hp}]`
Class `INPUT[inlineSelect(option(rogue, Rogue), option(mage, Mage)):class]`
Health `VIEW[round({hp} / {max_hp} * 100)][math:pct]`%
````

renders as

```text
Alive [x] · HP 7 / 10 / 10
Class Rogue
Health 70%
```

- **Inputs** show their current value: `[x]`/`[ ]` for toggles, `7 / 10` for sliders and
  progress bars, the option's label for selects and suggesters, and a comma list for lists. HTML
  output shows disabled form controls. Missing or mistyped values show the input's default
  (`defaultValue`, or the type's own), which is not written.
- **Views** (`VIEW[template][type:target]`): `math` (the default) evaluates the template with
  mathjs, `text` joins the values, `link` and `image` turn them into links and embeds, and
  `hidden` shows nothing. A view with a target passes its result on: later views in the note see
  it.
- **Bind targets**: `name`, `stats.hp`, `list[0]`, `["name with spaces"]`, and `Other Note#prop`
  for another note's frontmatter. `memory^name` and `globalMemory^name` hold values only while the
  note renders, as they hold them only while Obsidian has it open.
- **Buttons** show their labels; their actions run in Obsidian. `meta-bind-embed` blocks embed
  the note they name, and `meta-bind-js-view` blocks show a notice.
- Fields in `metabind.excluded_folders` (`templates` by default) show a notice, so templates keep
  their declarations.

Rendering never writes. Change values with

```sh
vulcan metabind list Hero                     # each field, its target, value, and diagnostics
vulcan metabind set Hero hp 3 --dry-run       # checked against every input bound to `hp`
vulcan metabind set Hero class mage
vulcan metabind apply Hero                    # writes view results, such as pct: 70
```

`set` refuses a value an input bound to the target would not accept (a slider's range, a select's
options, a list's limit) and targets no input binds. Both commands change only the properties they
write, keep the rest of the frontmatter as written, and run the vault's write hooks and
auto-commit.

## Numerals calculator blocks

The [Numerals](https://github.com/gtg922r/obsidian-numerals) plugin turns a ` ```math ` block into
a calculator: each line is evaluated by [mathjs](https://mathjs.org), with units, currencies,
functions, and variables. Vulcan bundles the same mathjs release and evaluates the same syntax. It
runs in an engine with no vault access, so blocks work in any vault, trusted or not.

Turn it on for the vault (other tools render ` ```math ` as LaTeX, so it is off by default):

```sh
vulcan config set numerals.enabled true
# or, in a vault that uses the Obsidian plugin:
vulcan config import numerals
```

A note:

````markdown
---
numerals: all
rate: $150
time: 3.75
---

```math
cost = rate * time     # what the client pays
tax = cost * 19%
$total = @sum
2 km + 300 m to mile
```

The client owes `#: $total`, and twice that is `#=: @prev * 2`.
````

renders as

````markdown
```text
cost = rate * time   → 562.50 USD  # what the client pays
tax = cost * 19%     → 106.88 USD
$total = @sum        → 669.38 USD
2 km + 300 m to mile → 1.429 mile
```

The client owes 669.38 USD, and twice that is @prev * 2 = 1,338.75 USD.
````

What works as in the plugin:

- **Styles:** `math` uses `numerals.render_style`; `math-plain`, `math-tex`, and `math-highlight`
  pick one per block. TeX blocks render as display math.
- **Directives:** `@prev` (the previous line's result), `@sum`/`@total` (the lines since the last
  blank line or comment), `@format period-comma` and `@decimalPlaces 2` for the block, and
  `@hideRows`.
- **Emitters:** when lines end in `=>`, only those lines show.
- **Currencies:** `$`, `€`, `£`, `¥`, and `₹` before a number become USD, EUR, GBP, JPY, and INR
  units (`numerals.dollar_currency` and `numerals.yen_currency` remap two of them), and
  `,` thousands separators are dropped, as the plugin drops them.
- **Scope:** frontmatter properties whose names start with `$` are always variables; others are
  when the note's `numerals` property says so (`all`, one name, or a list), or with
  `numerals.force_all_frontmatter`. Inline fields count as properties. A key like `f(x): x^2`
  defines a function. A block's variables stay in the block, except those starting with `$`,
  which every later block and inline calculation in the note sees.
- **Sums:** `@sum` adds the results since the last blank line or comment line; mixing plain
  numbers and currency amounts in one run is a summing error, as in the plugin.
- **Other notes:** `[[Prices]].$base` reads a property another note exposes (with `$` or its
  `numerals` property). Under a permission profile, notes the profile cannot read do not resolve.
- **Inline:** `` `#: expr` `` shows the result, `` `#=: expr` `` shows `expr = result`, and
  `` `#$: expr` `` and `` `#$=: expr` `` show them as TeX. `@prev` refers to the previous inline
  result in the same paragraph.

Vulcan has no locale database in its JavaScript engine, so the `system` number format is the
comma-thousands, period-decimal one. Pick another with `numerals.number_format` or `@format`.

### Writing results into the note

A line such as `@[total::0] = cost + tax` defines `total` and asks for its value to be written into
the brackets. Obsidian rewrites the line while you view the note; Vulcan never writes while
rendering. Run:

```sh
vulcan numerals apply Invoice --dry-run   # shows each line it would change
vulcan numerals apply Invoice             # writes the values, such as @[total::669.38 USD]
vulcan numerals eval Invoice              # shows every block's results
```

`apply` runs the vault's write hooks and auto-commit like any other note write.
