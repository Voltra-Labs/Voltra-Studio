# 008 — Compositor CPU

- **Fase:** 2
- **Estado:** propuesto

## Objetivo

Convertir una escena en un frame. `CpuCompositor` recorre los items visibles,
coloca cada fuente con su transformación, la muestrea y la mezcla sobre el
lienzo. Con imágenes doradas que fijan el resultado y benchmark del coste por
frame a 1080p.

## Fuera de alcance

- **GPU.** El backend `wgpu` es el destino (ADR 0001) y llega después; este es
  el camino de referencia con el que se validará aquel.
- **Fuentes en formatos YUV.** El compositor trabaja en BGRA. Una fuente que
  entregue I420 o NV12 necesita la conversión inversa, que no existe (plan 004
  hizo solo RGB→YUV). Esos items se cuentan como no soportados, no se dibujan
  mal ni se ignoran en silencio.
- **Filtros de escalado caros.** `Point` y `Bilinear` se implementan; `Bicubic`,
  `Lanczos` y `Area` se resuelven como bilineal, anotado en la documentación.
  `Area` importará de verdad al reducir mucho y llegará con su medición.
- Filtros de fuente encadenados y transiciones.
- Escalado de salida y conversión final: plan 009.

## Referencia

De `docs/references/obs-studio.md` §2, el orden del pipeline de vídeo en
`obs_graphics_thread`:

1. `tick_sources()` — todas las fuentes avanzan **antes** de dibujar nada.
2. `render_main_texture()` — limpia el destino, fija proyección ortográfica del
   tamaño del lienzo, dibuja los canales de salida.
3. `render_output_texture()` — reescalado a la resolución de salida.
4. `render_convert_texture()` — conversión RGB→YUV.
5. `stage_output_texture()` — descarga a CPU con doble búfer.

**Qué copiamos:** el orden. En particular que **escalar va antes de convertir**,
que es el arreglo gratuito de la deuda §3.1 de `docs/PERFORMANCE.md` (2,2×
medido al bajar de 1080p a 720p). Y el tick separado del dibujo, que evita que
una fuente cambie de estado a mitad de una composición.

**Qué mejoramos:**

1. **El compositor no puede recursionar.** En OBS una escena es una fuente, así
   que dibujar una escena anidada es una llamada recursiva, y hace falta
   comprobar ciclos al añadir hijos. Aquí el compositor **no resuelve
   `SourceId` a fuentes**: pide frames a un `FrameProvider`. Una escena anidada
   se compone antes, a su propio frame, y entra como un frame más. El riesgo que
   el plan 007 dejó abierto desaparece por construcción, no por una
   comprobación que alguien puede olvidar.
2. **Sin ramas por píxel.** El modo de mezcla y el filtro se resuelven **una vez
   por item**, despachando a una función monomórfica. En un bucle por píxel un
   `match` sobre siete modos es una rama que el predictor acierta siempre pero
   que impide vectorizar (CLAUDE.md §4.4).
3. **Contadores desde el primer día**: items dibujados, saltados, no soportados
   y tiempo de composición. CLAUDE.md §4.10.

## Diseño

Crate nueva `voltra-render`, que depende solo de `voltra-core`.

```rust
pub trait FrameProvider {
    fn frame(&self, source: SourceId) -> Option<&VideoFrame>;
}

pub trait Compositor {                      // ADR 0001: la GPU entrará por aquí
    fn composite(&mut self, scene: &Scene, sources: &dyn FrameProvider) -> Result<&VideoFrame>;
}

pub struct CpuCompositor { canvas: VideoFrame, background: [u8; 4], stats: CompositeStats }
pub struct CompositeStats { drawn, skipped, unsupported, last_duration }
```

El bucle, por item:

1. `transform.place(tamaño de la fuente)` → `Placement` (plan 002). `None` =
   nada que dibujar, se salta.
2. Recorte de la caja envolvente contra el lienzo. Solo se recorren los píxeles
   que pueden acabar dentro.
3. Para cada píxel de destino, **mapeo inverso** con `placement.inverse` →
   coordenada en la fuente. Fuera del rectángulo recortado, se salta.
4. Muestreo: vecino más cercano o bilineal con sujeción en los bordes.
5. Mezcla según el modo, con alfa recto: `dst = src·a + dst·(1−a)`.

Todo el trabajo caro —trigonometría, inversión de matriz, elección de modo y
filtro— queda **fuera** del bucle de píxeles.

## Pasos

1. Crate `voltra-render` con `FrameProvider`, `Compositor` y `CompositeStats`.
2. Muestreo: `Point` y `Bilinear`.
3. Mezcla: los siete modos, monomórficos.
4. `CpuCompositor::composite`.
5. `benches/composite.rs`.
6. Actualizar `docs/PERFORMANCE.md`.
7. Puerta de calidad.

## Verificación

Imágenes doradas sobre lienzos pequeños, comparadas píxel a píxel contra
matrices escritas a mano en el propio test:

- Lienzo vacío = color de fondo, alfa incluido.
- Una fuente opaca 1:1 sin transformación reproduce la fuente exactamente.
- Una fuente a media opacidad sobre fondo negro da exactamente la mitad.
- Un item desplazado deja el fondo intacto donde no llega, sin filtrarse.
- Un item que sale del lienzo se recorta sin desbordar ni entrar en pánico.
- Rotación de 90° coloca las esquinas donde deben ir.
- Escalado ×2: `Point` duplica píxeles exactos; `Bilinear` interpola.
- Los siete modos de mezcla, cada uno con un caso conocido.
- El orden importa: dos items opacos superpuestos, gana el último.
- Un item invisible no se dibuja; uno con fuente ausente cuenta como saltado.
- Una fuente en I420 cuenta como no soportada y **no** corrompe el lienzo.

Criterio de aceptación: **escena de 5 items a 1080p por debajo de 4 ms**, que es
el presupuesto que CLAUDE.md §4 reserva a la composición.

## Riesgos

- **Puede no cumplirse el presupuesto.** El muestreo bilineal son cuatro lecturas
  y ocho multiplicaciones por píxel; a 2 Mpx son 16 M lecturas por capa. Si sale
  por encima, se mide, se anota en `docs/PERFORMANCE.md` y se decide con datos —
  como en el plan 004, sin mover la portería.
- **Camino rápido tentador.** Un item sin rotación a escala 1:1 podría resolverse
  con `copy_from_slice` por filas. Es probablemente un orden de magnitud, pero
  **no se escribe antes de medir el caso general**: es exactamente la regla de
  CLAUDE.md §4.8.
