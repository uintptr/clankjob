## Markdown is formatted with mdformat

Every `.md` file in this tree — rule files, `README`s, skill and tool
documentation — is formatted with
[mdformat](https://mdformat.readthedocs.io/). Run it on any markdown
file you write or modify, and **always with both plugins and
`--number`**:

```
uvx --with mdformat-gfm --with mdformat-frontmatter mdformat --number <file>
```

A re-run must then be a no-op; `--check` exits non-zero when a file is
not formatted:

```
uvx --with mdformat-gfm --with mdformat-frontmatter mdformat --number --check <file>
```

Neither plugin nor the flag is a preference — each is a way to lose
content, and all three are covered below. Nothing in the repository
pins them, so the `--with` arguments are the whole safety: a bare
`uvx mdformat` over this tree destroys every table and every skill's
frontmatter.

### `mdformat-gfm` is not optional

Plain `mdformat` does not know what a GFM table is. It parses one as an
ordinary paragraph and **silently destroys it** — the rows are joined
into a single line of prose and the table is gone from the rendered
output:

```
| a     | bbbb |        becomes      | a | bbbb | | --- | --- | | ccccc | d |
| ----- | ---- |
| ccccc | d    |
```

`mdformat-tables` is the minimum that avoids this; `mdformat-gfm`
carries it plus the rest of the GFM syntax these files use (task lists,
strikethrough, autolinks), so it is the one to install.

### `mdformat-frontmatter` is not optional either

The `.claude/skills/*/SKILL.md` files open with a YAML frontmatter
block, and that block is the skill's identity — `name`, `description`,
`argument-hint`. Core mdformat does not know what frontmatter is: it
reads the opening `---` as a thematic break and the keys beneath it as
a setext heading, collapsing all of them onto one line.

```
---                                  ______________________________________
name: example               becomes
description: Review …                ## name: example description: Review …
---
```

The metadata is gone and the skill no longer loads. With the plugin the
block comes back byte-identical — including an `argument-hint` whose
`[a, b] (c)` value is not valid YAML and draws a parse _warning_; the
block is still passed through untouched, so the warning is advisory.

### `--number` keeps step numbers readable

mdformat's default renders every item of an ordered list as `1.`. HTML
renderers still number them 1, 2, 3 — but these documents are read as
raw text, so a nine-step procedure becomes nine step-ones and prose
that says "step 7" points at nothing. `--number` writes consecutive
numbers, and correctly continues a list that a paragraph interrupts
(resumes at 6, not 1).

A list of ten or more items is then zero-padded — `01.` through `09.`
ahead of `10.` — so the text after the markers lines up, the same
principle as the table padding. Some lists are written that way for
that reason.

### Tables are aligned

mdformat pads every cell so the pipes line up and each row is the same
length. Write the table however is convenient — a minimal `|---|---|`
separator is fine — and let the formatter square it up:

```markdown
<!-- write this -->

| alias  | C99 type   |
| ------ | ---------- |
| `Byte` | `uint8_t`  |
| `Word` | `uint16_t` |

<!-- mdformat produces this -->

| alias  | C99 type   |
| ------ | ---------- |
| `Byte` | `uint8_t`  |
| `Word` | `uint16_t` |
```

The padding is counted in **characters, not display columns**. These
documents are full of em dashes and arrows, which are one character
each, so they align correctly — but a table containing CJK or emoji
will look ragged in a terminal while still being correctly formatted.
Do not hand-pad such a table back into visual alignment; the next
mdformat run undoes it.

### Prose wrapping is left alone

The default `--wrap keep` preserves the line breaks already in a
paragraph, which is what keeps these hand-wrapped (~76 column)
documents from being reflowed into one line per paragraph. Do not pass
`--wrap no` or a column number.

What mdformat _does_ normalize: bullet markers (`*` → `-`), emphasis
markers, list continuation indentation, heading and fence style, and
trailing whitespace. Fenced code blocks and inline code are passed
through verbatim, so C examples are safe.

One normalization has no opt-out: a `---` thematic break is rewritten
as a row of 70 underscores. It renders identically and mdformat
exposes no setting for the style, so a document that uses `---` as a
section separator will carry those rows after its first format.

### Formatting every file: scope it with git, not with a directory

`mdformat .` walks the working tree, including `build/` output, tool
caches, and the markdown of every vendored dependency. `--exclude` can
pare that back, but it means maintaining a blocklist that a new build
directory defeats. `git ls-files` is the allowlist instead, and honours
`.gitignore` for free:

```
git ls-files -z '*.md' ':!:vendor/*' \
    | xargs -0 uvx --with mdformat-gfm --with mdformat-frontmatter mdformat --number
```

`vendor/` is excluded because it is vendored: reformatting it puts
this project's conventions into files that belong to another project
and makes the next version bump a merge conflict. Swap `mdformat` for
`mdformat --check` to ask the question without writing anything.

### Do not sweep the tree

Once the tree has been swept, `--check` passes on every tracked file
outside `vendor/`, and the formatting pass changes no content — the
alphanumeric token multiset of each file is identical before and after,
the sole exception being the zero-padded list markers above.

Because the tree is clean, keeping it that way is per-file — format
what you are already writing or changing, and a re-run over the rest is
a no-op. Do not open a standalone "reformat the docs" patch; there is
nothing left for one to do, and if a future plugin or version bump
changes the output, that churn belongs in its own commit with the
version that caused it named.
