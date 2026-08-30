# ADR 0001 — Backend de render: CPU de referencia primero, `wgpu` después

- **Estado:** aceptada (2026-08-30)
- **Fase:** 2

## Contexto

OBS compone en GPU: dibuja los canales de salida sobre una textura, reescala,
convierte RGB→YUV renderizando cada plano y descarga a CPU con doble búfer
asíncrono (`docs/references/obs-studio.md` §2). Ese es el destino correcto.

Pero el entorno de desarrollo y CI es un contenedor **headless sin GPU**, y sin
un camino verificable no hay forma de saber si el compositor es *correcto* antes
de optimizarlo. Además OBS mismo mantiene rutas de respaldo a memoria de sistema
cuando el compartido de texturas no está disponible.

## Opciones

1. **GPU-first con `wgpu` como único camino.** Máximo rendimiento antes, pero
   imposible de compilar y testear en CI headless; cada paso dependería de una
   validación manual.
2. **Solo CPU con SIMD.** Simple y testeable, pero techo bajo: 4K y escenas con
   muchas capas no entran en presupuesto.
3. **CPU de referencia + `wgpu` detrás del mismo trait.**

## Decisión

Opción 3. `voltra-render` expone un trait `Compositor` con dos
implementaciones intercambiables:

- `CpuCompositor`: correcto, portable, sin dependencias del sistema. Es el
  **oráculo** contra el que se validan los tests de imagen dorada y el respaldo
  real cuando no hay GPU utilizable.
- `GpuCompositor` (feature `gpu`, `wgpu`): el camino de producción, con
  conversión de color en GPU y descarga asíncrona con doble búfer, como libobs.

El trait se diseña **para GPU desde el principio** —trabajo por lotes, sin
llamadas por píxel cruzando la frontera, superficies opacas tipo *handle*— para
que el backend acelerado no obligue a rediseñarlo.

## Consecuencias

- `cargo test` con features por defecto sigue funcionando headless.
- Toda optimización del compositor se compara contra el mismo conjunto de
  imágenes doradas: la GPU no puede "mejorar" cambiando el resultado.
- Coste asumido: dos implementaciones que mantener. Se acota exigiendo que la
  lógica de geometría y color viva en `voltra-core`, compartida por ambas.
