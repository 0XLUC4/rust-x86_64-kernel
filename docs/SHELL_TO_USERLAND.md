# Migration : sortir le shell du kernel

État : design figé, implémentation à venir (chantier ~2-3 sessions).

## Contexte

Aujourd'hui, le shell (`kernel/src/shell/`) tourne en ring 0 :
- Il lit le clavier directement via la queue `KeyStream`.
- Il appelle `print!` qui écrit dans `drivers::vga` ou `drivers::fb`.
- Il exécute les builtins (`ls`, `cat`, `ping`, etc.) en accédant aux APIs kernel sans syscall.
- Quand on tape `exec /bin/foo`, il fait `process::execve_path()` qui charge l'ELF et iretq en ring 3, puis le kernel sauvegarde un `KernelCtx` pour pouvoir reprendre le prompt après l'exit du process user.

Tant que tout vit dans le kernel, le shell est **privilégié** : un bug dans le parser de `ls` peut corrompre la heap kernel.

## Cible

Le shell devient `/bin/sh`, un binaire ulib, lancé par `init` au boot. Il :
- lit `stdin` via `read(0, …)` syscall (clavier déjà routé par le kernel quand le fb_owner est displayd).
- écrit `stdout` via `write(1, …)`.
- pour chaque commande externe : `fork()` + `exec(path)` + `wait()`.
- les builtins (`cd`, `exit`, `echo`) restent in-process.
- les commandes kernel-réservées (`mem`, `ps`, `acpi`, `lspci`, `lspart`, etc.) deviennent des binaires séparés `/bin/mem`, `/bin/ps`, etc., qui appellent des syscalls d'introspection (à ajouter).

## Étapes

1. **Inventaire des dépendances kernel du shell actuel**
   - `shell::execute()` accède directement : `fs::*`, `process::*`, `net::*`, `time::*`, `acpi`, `pci`, `drivers::ata`, `drivers::part`, `drivers::fat32`.
   - Chacune doit devenir un syscall ou un binaire user dédié.

2. **Nouveaux syscalls d'introspection** (numéros 50-69 réservés)
   - 50 `SYS_MEM_INFO`   : total, free, used (KiB).
   - 51 `SYS_PS_LIST`    : liste des PID avec name/state.
   - 52 `SYS_NET_IFCFG`  : MAC, IP, gw, RX/TX bytes.
   - 53 `SYS_NET_STATS`  : socket table.
   - 54 `SYS_ACPI_DUMP`  : tables ACPI résumées.
   - 55 `SYS_PCI_LIST`   : devices PCI.
   - 56 `SYS_PART_LIST`  : partitions MBR.
   - 57 `SYS_FAT_LIST`   : entrées de la racine FAT32 montée.

3. **Binaire `/bin/sh`** : déjà esquissé dans `userland/sh/`. À étendre :
   - parser pipes `|` (créer 2 PIDs + pipe SHM ou IPC).
   - parser redirections `>` `<` (require open() syscall + fd table).
   - historique (Ctrl-R) → ring buffer en stack user.

4. **Suppression** : `kernel/src/shell/` est purement déplacé en user. Le kernel n'a plus de console interactive — il a juste son log série pour le debug.

## Bénéfices

- **Isolation** : un crash de `sh` ne touche pas le kernel.
- **Restartable** : `init` peut respawn `sh` (équivalent `getty` Unix).
- **Multi-instance** : N shells lancés en parallèle sont possibles dès qu'on a un pty / TTY virtuel (Phase VI).

## Coût estimé

- Syscalls d'introspection : ~1 jour.
- Re-écriture des 35 builtins en binaires séparés : ~2 jours.
- Câblage `init` → spawn `displayd` → spawn `sh` : ~½ journée.
- Total réaliste : **1 semaine en sessions de 4h**.

## Décision

Reporté à après Phase V step 3 (FB user-mapped fonctionnel + DHCP/DNS).
Le shell kernel reste en place comme **fallback de récupération** quand
displayd plante — utile pour debugger jusqu'à ce que `init` soit robuste.
