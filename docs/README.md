# HorizonDB documentation

Source of the documentation site published at
<https://ihrz.gitlab.io/db/docs/> (English) and
<https://ihrz.gitlab.io/db/docs/fr/> (French), built with
[Material for MkDocs](https://squidfunk.github.io/mkdocs-material/).

```
docs/
├── mkdocs.en.yml        # English site: docs_dir en/, site_dir ../public/docs
├── mkdocs.fr.yml        # French site:  docs_dir fr/, site_dir ../public/docs/fr
├── en/                  # English pages, assets/logo.svg, stylesheets/brand.css
└── fr/                  # French pages (same file names and heading ids)
```

Both languages have the same pages and the same heading ids (`{ #id }`), so a
link like `operations.md#memory-tuning` works in both. Keep them in step: a
change to one language goes to the other in the same commit. `assets/logo.svg`
is a copy of the repository's `assets/logo.svg`, and `stylesheets/brand.css`
is duplicated in `en/` and `fr/`: update both.

## Build

From the repository root, with only `mkdocs-material` installed:

```bash
pip install mkdocs-material
mkdocs build --strict -f docs/mkdocs.en.yml    # writes public/docs
mkdocs build --strict -f docs/mkdocs.fr.yml    # writes public/docs/fr
```

Build English **first**: each build empties its own output directory, and
`public/docs/fr` lives inside `public/docs`.

`site_dir` is resolved relative to the config file, so the explicit form used
by CI is equivalent:

```bash
mkdocs build -f docs/mkdocs.en.yml -d ../public/docs
mkdocs build -f docs/mkdocs.fr.yml -d ../public/docs/fr
```

`--strict` turns every warning (broken link, missing anchor, page not in the
nav) into an error.

## Preview

```bash
mkdocs serve -f docs/mkdocs.en.yml    # http://127.0.0.1:8000
```

The language switcher links to the published paths (`/db/docs/`,
`/db/docs/fr/`), so it only works once deployed.
