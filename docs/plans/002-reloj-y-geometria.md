# 002 — Reloj racional y geometría de colocación

- **Fase:** 1
- **Estado:** propuesto

## Objetivo

`voltra-core` gana los dos cimientos que todo lo demás usará: un **reloj
racional** con el que 29,97 fps no puede derivar ni tras horas de emisión, y la
**geometría de colocación** de una fuente sobre el lienzo (recorte, escala,
rotación, alineación, caja contenedora), incluida la matriz inversa que el
compositor necesitará para muestrear. Con arnés de benchmarks montado y en uso.

## Fuera de alcance

- Formatos de píxel, `Frame` y conversiones de color → plan 003.
- Grafo de escena, `SourceId` y los traits `Source`/`Filter`/`Output` → plan 004.
- `serde`. No hay todavía nada que serializar; la dependencia entra en el plan
  que traiga la configuración de escenas, no antes.
- Cualquier composición real de píxeles.

## Referencia

De `docs/references/obs-studio.md` §1 y §2:

- **`obs_transform_info`** guarda posición, rotación, escala, **alineación** y
  **bounds**; **`obs_sceneitem_crop`** guarda el recorte por lado. OBS resuelve
  los bounds con siete modos (`OBS_BOUNDS_NONE`, `STRETCH`, `SCALE_INNER`,
  `SCALE_OUTER`, `SCALE_TO_WIDTH`, `SCALE_TO_HEIGHT`, `MAX_ONLY`).
- **`obs_video_info`** expresa la cadencia como `fps_num`/`fps_den`, no como
  decimal.

**Qué copiamos:** el modelo completo de transformación (recorte → escala →
alineación → bounds → rotación → posición) y sus siete modos de bounds. Está
probado por años de uso y es lo que los usuarios de OBS ya tienen en la cabeza;
inventar otro modelo solo aportaría confusión.

**Qué mejoramos:**

1. OBS compone en GPU y le basta la matriz directa. Nuestro camino CPU de
   referencia (ADR 0001) muestrea por **mapeo inverso**: para cada píxel de
   destino pregunta de dónde viene. Por eso `Placement` expone **directa e
   inversa** ya calculadas y validadas una sola vez por frame, en vez de
   invertir dentro del bucle.
2. El *pts* de cada frame se calcula desde el **índice absoluto** en aritmética
   entera de 128 bits (`index * den * 1e9 / num`), no acumulando duraciones.
   Acumular es la causa clásica de deriva; aquí es imposible por construcción.
3. Una transformación degenerada (escala cero, recorte que se come la fuente) se
   representa como `Option::None` en el tipo, no como un rectángulo vacío que
   luego alguien olvida comprobar.

## Diseño

### `time`

| Tipo | Invariante |
|---|---|
| `Timestamp(u64)` | Nanosegundos desde el inicio del stream. Monótono. |
| `Fps { num, den }` | `den != 0`. `pts(index)` exacto, sin acumulación. |

`Fps` implementa `FromStr` aceptando `60`, `30000/1001` y `29.97` (las dos
cadencias NTSC decimales se ajustan a su fracción exacta: quien escribe `29.97`
quiere 30000/1001). `Timestamp` se muestra como código de tiempo
`HH:MM:SS.mmm`.

### `math`

`Vec2`, `Rect`, `Anchor` (9 posiciones), `BoundsMode` (los 7 modos de OBS),
`Crop`, `Transform` y `Affine` (matriz 2×3).

`Transform::place(size) -> Option<Placement>` resuelve la transformación contra
un tamaño de fuente concreto y devuelve:

```rust
pub struct Placement {
    pub src: Rect,       // rectángulo de origen que sobrevive al recorte
    pub forward: Affine, // origen -> lienzo   (para calcular la caja envolvente)
    pub inverse: Affine, // lienzo -> origen   (para muestrear)
    pub bbox: Rect,      // caja envolvente en el lienzo, ya rotada
}
```

Decisiones de rendimiento:

- **`f32`, no `f64`**: la mitad de ancho de banda de memoria, el doble de
  carriles SIMD, y precisión de sobra para coordenadas de lienzo (un `f32`
  representa exactamente todos los enteros hasta 2^24 = 16,7 millones).
- `place()` se llama **una vez por item y frame**, nunca por píxel: todo el
  trabajo caro (seno, coseno, inversión de matriz) queda fuera del bucle.
- Todos los tipos son `Copy` y sin asignaciones: caben en registros.

## Pasos

1. `time.rs`: `Timestamp` y `Fps` con sus tests de invariantes.
2. `math.rs`: geometría y `place()` con sus tests.
3. `benches/geometry.rs` con `criterion`: número para `place()` y `pts()`.
4. Reexportar desde `lib.rs` y actualizar `voltra info` si procede.
5. Puerta de calidad y anotación de resultados en este plan.

## Verificación

Tests que deben existir y pasar:

- `Fps::pts` exacto en cadencias enteras y en NTSC: una hora de 29,97 son
  107 892 frames y **exactamente** 3 603,6 s. Sin deriva.
- `FromStr` acepta las tres formas y rechaza `0`, `1/0` y basura.
- `Timestamp` se formatea como código de tiempo.
- La inversa compuesta con la directa devuelve el punto original (ida y vuelta).
- Anclaje centrado coloca la fuente centrada en la posición dada.
- `BoundsMode::Inner` mete 16:9 en una caja cuadrada **sin deformar** y con
  barras iguales arriba y abajo; `Outer` cubre la caja y desborda.
- El recorte reduce el área visible sin mover el origen.
- Escala cero y recorte excesivo devuelven `None`.

Criterio de aceptación medible: `place()` por debajo de **1 µs** (a 60 fps con
100 items serían 6 ms/s solo en geometría, inaceptable si se acercara).

## Riesgos

- **`clippy::pedantic` y los casts.** Código geométrico convierte entre enteros y
  flotantes constantemente. Se resolverá con `#[allow]` **local y justificado**
  en los puntos donde la pérdida de precisión sea intencionada, nunca a nivel de
  crate.
- **Precisión de `f32` en lienzos grandes.** A 8K (7680 px) el epsilon relativo
  sigue siendo ~0,0005 px: irrelevante. Si algún día se compone en un espacio
  mucho mayor, se revisa; queda anotado aquí para no redescubrirlo.
- **`criterion` alarga CI** al compilarse con `--all-targets`. Se asume: sin
  arnés de medición, la regla "medir antes y después" no es aplicable, y la
  prioridad del proyecto es el rendimiento.
