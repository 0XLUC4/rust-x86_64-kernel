# Higher-half kernel + KASLR

État : **design figé, implémentation reportée** — touche boot.asm,
linker.ld et chaque adresse absolue. Sans boucle QEMU pour itérer,
risque très élevé de boot dead.

## Layout actuel

```
0x0000_0000_0010_0000 ─── kernel _start (1 MiB)
0x0000_0000_4000_0000 ─── user ELF entry (1 GiB)
0x0000_0000_7fff_ffff ─── user stack top
                  ...
0x0000_0000_FEC0_0000 ─── IOAPIC MMIO
0x0000_0000_FEE0_0000 ─── LAPIC MMIO
```

Le kernel vit dans la moitié basse, identity-mappée sur 1 GiB via huge
pages 2 MiB depuis `boot.asm`.

## Problèmes du layout actuel

1. **Pas d'isolation** : un pointeur user qui pointe vers le kernel
   (e.g. via un bug de validation `user_slice`) accède au code kernel.
   Aujourd'hui sauvé par `USER_SPACE_MAX = 0x8000_0000_0000`, mais c'est
   un check soft.
2. **Pas de KASLR** : adresse `_start` connue → exploit de redirection
   facile si une vuln user-side leak un pointeur kernel.
3. **Conflits userspace** : un process user qui demande à mapper à
   1.5 GiB tape dans le kernel. On contourne en obligeant ELF entry à
   être au-delà de la zone kernel, mais c'est fragile.

## Layout cible (higher-half canonique)

```
0x0000_0000_0000_0000 ─┐
                        │  userspace (jusqu'à 128 TiB selon CPU)
0x0000_7fff_ffff_ffff ─┘
                          (gap canonique)
0xFFFF_8000_0000_0000 ─┐
                        │  zone kernel (TOUT le kernel ici)
0xFFFF_FFFF_FFFF_FFFF ─┘
```

Le kernel est mappé à partir de `KERNEL_BASE = 0xFFFF_8000_0000_0000`.
Le mapping `[0, 1 GiB)` identity est conservé temporairement pendant le
boot puis démappé.

## Étapes d'implémentation

### 1. Linker script (`kernel/linker.ld`)
- Remplacer `. = 0x100000;` par `. = 0xFFFF_8000_0010_0000;`.
- Ajouter une section `.boot` toujours à 1 MiB pour les stubs asm.

### 2. boot.asm (32-bit transition)
- Continuer à charger le kernel à 1 MiB physique (GRUB-imposed).
- Construire P4 avec :
  - `P4[0]` → identity-map 0..1 GiB (boot+early init)
  - `P4[256]` → higher-half kernel @ 0xFFFF_8000_0000_0000+
  - Les deux entries pointent sur la même P3 → kernel physique unique,
    virtuel disponible aux deux endroits.
- Au moment du saut en long mode, calculer `_start_higher = _start + 0xFFFF_8000_0000_0000` et jumper à l'adresse haute.

### 3. Adresses absolues
- Toutes les constantes `extern "C"` (handler IDT, syscall_entry) sont
  des labels asm qui doivent passer du link à des adresses higher-half.
- Tous les `PHYS_OFFSET` calculs : `virt = phys + 0xFFFF_8000_0000_0000`
  au lieu de `virt = phys + 0`.
- `paging::PHYS_OFFSET` constante actuellement = 0 ; passer à
  `0xFFFF_8000_0000_0000` et auditer chaque appelant.

### 4. Démapper l'identity
- Après que tous les init kernels soient passés à des adresses high,
  vider `P4[0]` (ou juste mettre PRESENT=0). Cela protège les premiers
  4 KiB (NULL pointer) et libère la moitié basse pour les processes user.

### 5. KASLR

Une fois higher-half stable :

- Au boot, BSP tire un offset aléatoire `slide ∈ [0, 1 TiB)` aligné à
  2 MiB depuis : `rdrand` (CPUID feature) ou `rdtsc` xor cpuid.
- Translate tout le kernel : `KERNEL_BASE = 0xFFFF_8000_0000_0000 + slide`.
- Le linker.ld reste à `0xFFFF_8000_0000_0000` mais le runtime relocate
  via PIE-style + relocs ELF.
- Coût : chaque saut absolu doit être patchable, donc compiler avec
  `-relocation-model=pic` + `-code-model=kernel`.

## Risques / surprises connues

- **Triple fault au saut en mode long** si le code haut n'est pas
  mappé. Le saut doit utiliser un `jmp [rax]` après calcul, pas un
  `jmp label_haut` direct (sinon assemblé en relative jump qui
  référence l'adresse basse).
- **GDT entries higher-half** : la GDT contient des sélecteurs flat
  (base=0, limit=4G), donc pas affectée. Mais le pointeur LGDT doit
  être en virtuel haut une fois passé en higher-half.
- **TSS** : alloué dynamiquement en Rust → naturellement en higher-half
  après le switch. OK.
- **Stack initiale** : `_start` arrive avec une stack 32-bit fournie par
  GRUB en basse mémoire. Switcher vers une stack en BSS kernel
  *immédiatement* en arrivant à `_start` 64-bit.

## Pré-requis qui manquent

- QEMU + nasm pour itérer.
- Session debug serial + GDB (-s -S) — quand le boot freeze on n'a
  *que* le serial pour comprendre.
- Patience : la 1ère version va certainement triple-fault 5-10 fois.

## Décision

Reporté à après que SMP soit en place. Higher-half + KASLR sont des
chantiers de 2-3 sessions de 4h chacun, dont une bonne moitié en
debugging triple faults via serial.
