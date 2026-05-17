# tools/

Scripts de build et helpers qui ne sont ni kernel ni userland.

## Prévu

- `mkdisk.sh` : génère `build/test.img` FAT32 avec fichiers de démo
- `mkinitrd.py` : pack un répertoire en archive maison pour `module2` GRUB
- `qemu-debug.sh` : lance QEMU avec les bons flags pour GDB + logs d'IRQ

Pour l'instant le Makefile kernel fait le mkdisk en inline. On extraira
ici quand ça grossit.
