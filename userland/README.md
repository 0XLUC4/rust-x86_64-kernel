# userland/

Programmes user ring 3. Pour l'instant les ELFs de démo (`hello_user`,
`counter`) sont générés programmatiquement par `kernel/src/fs/userprog.rs`
au boot et embarqués directement dans le ramfs.

## Prévu

- `init` : premier process démarré au boot, spawn un shell user
- `sh` : shell ring 3 avec parse + exec + fork + wait
- `cat`, `ls`, `echo`, `ps` : coreutils minimaux
- pipeline : une vraie libc minimaliste (open/read/write/close/fork/exec)

Tant qu'il n'y a pas de compilateur cross-target dans l'arbo, on génère
l'asm à la main. Dès qu'on a un `libkern-user` crate (no_std, target
`x86_64-unknown-none` user-style), on pourra écrire en Rust.
