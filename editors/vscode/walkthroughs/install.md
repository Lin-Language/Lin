# Install `lin` on your PATH

The extension bundles the `lin` compiler and is already on the PATH of VS Code's
integrated terminal. To use `lin` in **any** terminal, run the
**Lin: Install `lin` on PATH** command — it symlinks the bundled compiler into
`~/.local/bin`.

Then, from any shell:

```sh
lin run hello.lin
```
