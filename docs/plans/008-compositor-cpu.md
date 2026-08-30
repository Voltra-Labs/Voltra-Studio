# 008 — Compositor CPU

- **Fase:** 2
- **Estado:** completado con desviación (2026-08-30) — ver Resultado

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

**Qué adoptamos:** el orden. En particular que **escalar va antes de convertir**,
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

## Resultado

Puerta de calidad en verde: **124 tests** (87 en `voltra-core`, 15 en
`voltra-sources`, 14 imágenes doradas del compositor, 1 en `voltra-cli`, 7
doctests). Funciona todo lo previsto: siete modos de mezcla, dos filtros,
recorte, rotación, orden de capas, y los casos degenerados reportados en vez de
dibujados mal.

### El criterio de aceptación NO se cumple, por mucho

Escena de 5 items a 1080p: **33,0 ms**. El presupuesto era 4 ms. Ocho veces por
encima.

### Cuatro rondas de optimización, todas medidas

| Ronda | Hipótesis | Resultado (1 capa a pantalla completa) |
|---|---|---|
| Punto de partida | — | 110,2 ms |
| 1 | Las conversiones `f32 → i32` del bucle interno son saturantes en Rust y compilan a comparación + selección; el muestreo bilineal hace dieciséis por píxel. **Coordenadas en punto fijo 16.16.** | 51,4 ms (**−54 %**) |
| 2 | El `clamp` con `if/else if/else` mete dieciséis ramas por píxel. **`clamp` sin ramas** + atajo para píxeles opacos (premultiplicar por 255 es identidad; `Normal` opaco es una copia). | 36,9 ms (**−28 %**) |
| 3 | A escala 1:1 el bilineal lee cuatro téxeles cuyos pesos resuelven a uno solo. **Detectar el mapeo alineado a píxel y sustituir por vecino cercano**, que es *bit a bit idéntico*, no un cambio de calidad. | 11,5 ms (**−69 %**) |

Mejora acumulada: **9,6×**. Los tests doradas pasaron sin cambios en las tres
rondas, que es lo que permite optimizar sin miedo.

### Cifras finales

| Caso, 1080p | Coste | Presupuesto (4 ms) |
|---|---|---|
| Una capa a pantalla completa, 1:1 opaca | 11,5 ms | 2,9× |
| **Escena de 5 items (criterio)** | **33,0 ms** | **8,3×** |
| Escalado a pantalla completa, vecino cercano | 10,8 ms | 2,7× |
| Escalado a pantalla completa, bilineal | 36,9 ms | 9,2× |

### El veredicto, y por qué importa

Una capa opaca a escala 1:1 es **semánticamente una copia de memoria**. Copiar
esos 8,29 MB cuesta 410 µs (plan 003). Nosotros tardamos 11,5 ms: **28 veces
más** que el `memcpy` equivalente. Ese factor es la maquinaria por píxel —
comprobación de cobertura, sujeción de coordenadas, lectura byte a byte con
índices en tiempo de ejecución, mezcla y escritura— y no desaparece afinando.

**Esto confirma el ADR 0001 con datos, no con intuición: la GPU no es una
mejora opcional, es un requisito.** El camino CPU cumple lo que el ADR le pide
—ser correcto, portable y verificable en CI— y ahí se queda. Lo que no puede es
ser el camino principal a 1080p60.

Lo que queda sobre la mesa para CPU, en `docs/PERFORMANCE.md`, por si alguna vez
hace falta un respaldo usable: copia directa de filas para el caso alineado,
opaco y `Normal`; orden de canales como parámetro de tipo para que la lectura de
píxel sea una sola palabra; y SIMD. Ninguna se hace ahora: no cambian el
veredicto y la GPU las deja irrelevantes.

## Desviaciones respecto al plan

- El criterio de 4 ms no se cumple (arriba, con todo el detalle).
- Se implementó el atajo de 1:1 que el plan listaba como *riesgo* de
  optimización prematura. La diferencia es que llegó **después** de medir el
  caso general, con una mejora medida del 69 % y siendo bit a bit idéntico. La
  regla de CLAUDE.md §4.8 es "no optimices sin perfil previo", y hubo perfil.
- El muestreo pasó a punto fijo, no previsto: además de ser el 54 % de la
  primera ronda, un paso entero no acumula error, cosa que el `f32` no puede
  prometer.
- `RGBA8` se soporta además de `BGRA8`, con un swizzle en la lectura. Estaba
  planteado como "no soportado" y salía casi gratis.

## Riesgos

- **Puede no cumplirse el presupuesto.** El muestreo bilineal son cuatro lecturas
  y ocho multiplicaciones por píxel; a 2 Mpx son 16 M lecturas por capa. Si sale
  por encima, se mide, se anota en `docs/PERFORMANCE.md` y se decide con datos —
  como en el plan 004, sin mover la portería.
- **Camino rápido tentador.** Un item sin rotación a escala 1:1 podría resolverse
  con `copy_from_slice` por filas. Es probablemente un orden de magnitud, pero
  **no se escribe antes de medir el caso general**: es exactamente la regla de
  CLAUDE.md §4.8.
