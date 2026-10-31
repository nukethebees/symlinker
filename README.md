# symlinker

Automate creating symlinks from an explicit links file.
Supports Windows and Linux.
Requires rust.

## Install

Build and install to `<my_path>\bin\symlinker.exe`:

```sh
cargo build --release --locked
cargo install --path . --locked --root '<my_path>'
```

## File format

Place a links file (for example, `symlinker.links`) in the directory where the
symlinks should be created. Write one `name = target` entry per line:

```text
# Executables
ninja.exe = C:\Tools\ninja\ninja.exe
clang.exe = C:\Program Files\LLVM\bin\clang.exe

# Directories
scripts = tools\scripts
```

Link names may use mixed-case ASCII letters. Names must be unique; on Windows,
case is ignored when checking for collisions (for example, `Tool.EXE` and `tool.exe`).

## Use

```sh
symlinker 'C:\Users\Example\bin\symlinker.links' plan
symlinker 'C:\Users\Example\bin\symlinker.links' apply
symlinker 'C:\Users\Example\bin\symlinker.links' import 'C:\Users\Example\bin'
```

- `import` creates a new `.links` file from the source directory's immediate context
  - Existing files are not overwritten
- `plan` is a dry run
- `apply` executes the file

