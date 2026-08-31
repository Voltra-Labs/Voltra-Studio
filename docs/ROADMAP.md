# Voltra Studio — Roadmap

Fases del producto. Cada fase se descompone en pasos pequeños con su plan en
`docs/plans/`. No se empieza una fase sin cerrar la anterior.

## Fase 0 — Fundaciones
- ADRs de las decisiones abiertas (render, plataforma, UI, encoding).
- Esqueleto del workspace, lints compartidos, perfiles de compilación, CI.
- **Criterio de salida:** `cargo test`, `clippy -D warnings` y `fmt` en verde en
  un contenedor headless.

## Fase 1 — Vocabulario y geometría (`voltra-core`)
- Tipos de frame, formatos de píxel, reloj racional, transformaciones, grafo de
  escena, traits `Source`/`Filter`/`Output`, sistema de propiedades.
- **Criterio de salida:** cobertura de tests sobre geometría, color y timing.

## Próximos pasos comprometidos

**La secuencia 007–010 está cerrada.** El objetivo era llegar al primer hito
visible en tres pasos, y se llegó; el cuarto trajo el backend acelerado.

| Plan | Qué dejó funcionando | Estado |
|---|---|---|
| **007** | Grafo de escena: `Scene`, `SceneItem`, orden de capas, visibilidad, bloqueo | Hecho |
| **008** | Compositor CPU: escena → frame, mezcla alfa, muestreo por mapeo inverso, imágenes doradas | Hecho, con el criterio de rendimiento incumplido y documentado |
| **009** | Salida Y4M desde `voltra render` | **Hecho. Primer hito visible.** |

Desde aquí, cada paso se puede *ver*:

```bash
voltra render -o - --frames 300 | ffplay -
voltra render -o demo.y4m --size 1920x1080 --frames 600
```

| **010** | Backend GPU `wgpu` tras el mismo trait, más `voltra render --gpu` | Hecho, verificado contra el camino CPU |

### Qué viene ahora

La fase 2 **sigue sin cerrarse, y por un motivo concreto**: su criterio de
salida es 1080p60 dentro del presupuesto, y eso es una medición. El backend GPU
ya existe y es correcto (plan 010), pero se ha verificado sobre un rasterizador
software, así que **no hay ninguna cifra de GPU** (`docs/PERFORMANCE.md` §3.6).
Cerrar la fase 2 no requiere escribir código: requiere una máquina con GPU y una
tarde.

Mientras tanto la **fase 3** avanza: vocabulario de audio en `voltra-core`
(plan 011) y mezclador multipista en `voltra-audio` (plan 012). **El criterio de
salida de la fase ya está cumplido y demostrado con un test**: cero asignaciones
y cero bloqueos en la mezcla. Queda el resto de la fase —medidores y paneo,
remuestreo, y la sincronía A/V anclada al reloj de audio—, que es el siguiente
plan.

Después, por orden de utilidad: audio (fase 3), encoding real (fase 4) —que es
también lo que quita los 187 MB/s de Y4M—, captura con PipeWire (fase 5), el
momento en que el programa sirve para algo, streaming (fase 6) e interfaz
(fase 7).

Deuda de rendimiento pendiente en `docs/PERFORMANCE.md`.

## Fase 2 — Compositor
- Composición escena → frame con transformaciones, recorte, mezcla alfa y
  escalado de calidad. Camino CPU verificable + tests de imagen dorada.
- Backend GPU según el ADR de render.
- **Criterio de salida:** 1080p60 compuesto dentro del presupuesto, con
  benchmark publicado.

## Fase 3 — Audio
- Vocabulario: frecuencia, canales, formatos de borde, búfer float planar
  (plan 011, hecho).
- Mezclador multipista con ganancia interpolada y control sin bloqueos
  (plan 012, hecho).
- Medidores y paneo, remuestreo, sincronía A/V anclada al reloj de audio.
- **Criterio de salida:** mezcla sin asignaciones ni bloqueos en el callback.
  **Cumplido y demostrado** por `voltra-audio/tests/no_allocation.rs`, que
  cuenta asignaciones con un asignador global.

## Fase 4 — Salida a fichero
- Codificación de vídeo y audio, muxing, grabación real reproducible.
- **Criterio de salida:** grabar una escena durante minutos sin deriva A/V ni
  crecimiento de memoria.

## Fase 5 — Captura de plataforma
- Pantalla, ventana, cámara y audio del sistema en la plataforma prioritaria.
- **Criterio de salida:** captura a 60 fps con frames perdidos medidos y
  acotados.

## Fase 6 — Streaming
- Transporte en vivo (RTMP/SRT/WHIP), gestión de red, reconexión y bitrate
  adaptativo.

## Fase 7 — Interfaz
- Previsualización, editor de escenas, mezclador, ajustes, estadísticas en vivo.

## Fase 8 — Extensiones
- Registro de plugins con aislamiento, para que una extensión defectuosa no
  pueda tumbar el estudio.
