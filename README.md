# Refyard

Refyard is a Rust CLI and library that tracks declared dependencies on shared files and directories across repositories. Projects declare stable resource IDs in `refyard.yaml`; a machine registry maps those IDs and revisions to local paths. A disposable file index provides forward and reverse lookups without a database.

Refyard does not observe runtime filesystem usage, resolve versions, download resources, or create resource contents.

## Configure the machine

Refyard automatically uses the operating system's per-user configuration and state directories, sharing the same registry across repositories:

```sh
refyard registry list
```

Default locations follow the [platform directory conventions](https://docs.rs/directories/latest/directories/struct.ProjectDirs.html):

| Platform | Registry | State directory |
| --- | --- | --- |
| Linux | `~/.config/refyard/refyard.registry.yaml` | `~/.local/state/refyard` |
| macOS | `~/Library/Application Support/refyard/refyard.registry.yaml` | `~/Library/Application Support/refyard/state` |
| Windows | `%LOCALAPPDATA%\refyard\config\refyard.registry.yaml` | `%LOCALAPPDATA%\refyard\data\state` |

On Linux, system XDG configuration and state locations are honored when configured. The registry uses machine-local configuration storage on Windows. Optional `REFYARD_REGISTRY` and `REFYARD_STATE_DIR` environment variables override the system defaults. Global `--registry <PATH>` and `--state-dir <PATH>` flags take precedence over the corresponding environment variables. Relative overrides are based on the current directory. `template.env` documents optional overrides; Refyard reads exported variables without automatically loading `.env` files.

Commands automatically create an empty registry and its parent directory when missing. Existing registry contents are preserved, including when multiple processes attempt creation concurrently; malformed or unreadable registries still fail when read. Source files and generated state use atomic writes. An operating-system lock on `state.lock` serializes CLI operations sharing a state directory. The lock is for process synchronization, not dependency resolution.

## Declare project dependencies

Put a `refyard.yaml` file in each project:

```yaml
requires:
  - id: hf:sentence-transformers/all-MiniLM-L6-v2
    revision: abc123
  - id: hf:Qwen/Qwen2.5-7B-Instruct
    revision: def456
```

An identity is the exact pair of `id` and optional `revision`. Omitting a revision declares an unversioned identity; it does not match every registered revision. IDs and supplied revisions must be nonblank strings. Duplicate identities and unknown fields are rejected to catch configuration mistakes. A project with no dependencies declares `requires: []` explicitly.

## Map local resources

Edit the machine registry directly:

```yaml
artifacts:
  - id: hf:sentence-transformers/all-MiniLM-L6-v2
    revision: abc123
    path: /shared/models/minilm
    notes: Downloaded from Hugging Face
```

Paths may refer to files or directories. Relative paths are based on the registry file's directory, independently of the working directory. Notes are optional retrieval information. Missing locations are allowed in a hand-edited registry and reported by validation.

Alternatively, add an existing location with the CLI, which stores its canonical absolute path:

```sh
refyard registry add hf:sentence-transformers/all-MiniLM-L6-v2 \
  --revision abc123 --path /shared/models/minilm \
  --notes 'Downloaded from Hugging Face'
refyard registry list
refyard registry remove hf:sentence-transformers/all-MiniLM-L6-v2 --revision abc123
```

Removing a mapping leaves resource contents untouched. Adding an already registered identity fails; edit the registry to change its mapping. Distinct identities may share a location, and cleanup protects that location while any mapped identity is referenced.

## Discover projects and rebuild the index

Register projects individually, then read their manifests into the index:

```sh
refyard repo add /work/project-a /work/project-b/refyard.yaml
refyard repo list
refyard scan
```

Repositories do not need a `.git` directory. Registration accepts a project directory or its manifest and validates the manifest immediately.

Discover all manifests below one or more directory roots:

```sh
refyard scan /work /other/projects
refyard scan --respect-gitignore --ignore-hidden
refyard scan --full
```

Roots are saved, and every scan visits all saved roots plus all registered projects. New roots extend the existing scope. By default discovery includes hidden and Git-ignored entries. The two exclusion flags narrow discovery for that invocation; already known projects are still checked individually. `--respect-gitignore` honors `.gitignore`, repository excludes, and global Git ignore rules, including `.gitignore` files outside Git repositories.

`--full` requires at least one saved or supplied root and conflicts with either exclusion flag. A full scan succeeds only when traversal and every known manifest can be inspected successfully. A full scan means complete coverage of the configured roots, not an assertion that every directory on the machine was scanned. Choose roots containing every potential consumer; use `/` only if you intend a literal whole-filesystem traversal and can inspect all of it.

Discovery does not follow symlinked directories. Add their real targets as scan roots or register those projects directly. A symlinked manifest file is read normally. There is no depth limit; overlapping roots and repeated registrations are deduplicated.

The state directory contains:

| File | Purpose |
| --- | --- |
| `repositories.yaml` | Durable absolute repository locations and discovery roots, including discovered projects |
| `index.yaml` | Disposable, versioned dependency snapshot and the last scan's errors/full-scan status |
| `state.lock` | Operating-system synchronization for CLI processes |

Delete `index.yaml` at any time and run `refyard scan` to rebuild it. Rebuilding needs only durable discovery inputs and project manifests. Missing repositories remain registered and produce errors, so an unavailable drive cannot silently turn its resources into cleanup candidates. Use `refyard repo remove /work/retired-project` to explicitly forget a retired project. An existing manifest below a saved root will be rediscovered on the next scan. To retire a scan root, edit `repositories.yaml` deliberately and rebuild; registration changes invalidate the old index.

## Query and validate

```sh
refyard projects
refyard registry list
refyard refs hf:sentence-transformers/all-MiniLM-L6-v2
refyard refs hf:sentence-transformers/all-MiniLM-L6-v2 --revision abc123
refyard validate
refyard --json refs hf:sentence-transformers/all-MiniLM-L6-v2
```

`projects` shows forward dependencies. `refs` groups reverse lookups by exact identity; omitting its revision filter returns all revisions. Unknown IDs return an empty list, and declared IDs without registry entries still appear in reverse lookups. `registry list` shows each registered resource's mapping, current availability, and live known project dependents. It works without an index and reports any unreadable project manifests in `issues`, exiting with status 1 when reference information is incomplete.

`projects` and `refs` use the last index; rerun `scan` after changing manifests. `registry list` and `validate` read live manifests for known repositories without discovering new ones or authorizing deletion. Validation checks the current registry and reports unknown identities, unavailable resources, unavailable repositories or roots, and unreferenced resources. Unknown revisions count as unknown identities. Validation exits with status 1 when warnings exist; an unreferenced resource means no *known* project references it and does not alone establish deletion safety. Scan metadata and traversal errors are included in `scan` output.

Default output is YAML; `--json` provides structured JSON on stdout. Operation errors go to stderr. Exit status 0 indicates success, 1 indicates an operation failure, incomplete scan or registry listing, or validation warnings, and 2 indicates invalid CLI arguments.

User-facing errors follow [Eros's application error guidance](https://github.com/mcmah309/eros#exposing-errors-to-application-users): only explicitly approved error types and context marked with `user_context` are presented. Filesystem failures use safe messages based on error kind, and YAML failures show locations without echoing document contents. Unexpected failures receive a generic fallback. Use `--debug-errors` to additionally print full internal diagnostics for a failed command on stderr; this opt-in output may contain paths, source details, and backtraces. `RUST_BACKTRACE=1` alone does not expose diagnostic details in normal error output.

## Clean up unused resources

```sh
refyard scan --full /work /other/projects
refyard cleanup
refyard cleanup --delete
```

`cleanup` is a dry run: it lists only unreferenced locations from the registry, with reasons for blocked candidates and a separate list of already missing resources. It does not sweep arbitrary unregistered files. A dry run can use a partial index; entries are blocked when that scan has unresolved errors.

Deletion requires `--delete` and a prior successful full scan. It repeats an unfiltered scan of every saved root and registered project immediately before planning deletion. A newly added project or changed manifest is therefore taken into account. Any traversal failure, malformed manifest, missing repository, or unavailable root aborts deletion. A normal or failed rescan revokes previous full-scan authorization.

Deletion protects referenced locations and their ancestors/descendants, even when different IDs or symlinks alias the same resource. It also protects known project contents, manifest targets, the registry, Refyard state, scan roots, and ancestors of the current working directory. Symlink resources and special filesystem objects are blocked. Eligible files are removed and eligible directories are removed recursively; nested and duplicate locations are deleted once. Blocked candidates remain in the report while other eligible candidates may be deleted.

Cleanup consumes the index authorization before deleting anything. Scan again to perform later lookups or another cleanup. Registry entries remain authoritative and are not removed automatically, so deleted locations subsequently appear as missing until you edit their mappings or remove the entries.

Keep the registry, project manifests, and resource paths stable during deletion. Refyard's state lock coordinates its own CLI processes sharing that state directory; it cannot lock out external filesystem changes. Full-scan safety depends on configured coverage and declared references, and cannot establish whether an undeclared runtime consumer uses a resource.
