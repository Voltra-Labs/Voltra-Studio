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

Secuencia acordada, en este orden y sin desviarse. El objetivo es llegar al
**primer hito visible** —un fichero de vídeo reproducible— en tres pasos.

| Plan | Qué deja funcionando | Por qué ahora |
|---|---|---|
| **007** | Grafo de escena: `Scene`, `SceneItem`, orden de capas, visibilidad, bloqueo, anidamiento | Es el pegamento entre geometría (002), frames (003) y fuentes (006). Sin bucles calientes |
| **008** | Compositor CPU: escena → frame, mezcla alfa, muestreo por mapeo inverso, imágenes doradas | El paso gordo de la fase 2. Valida si las decisiones de los planes 002–006 estaban bien tomadas |
| **009** | Salida Y4M desde `voltra render` | **Primer hito visible.** Y4M es vídeo crudo con cabecera de texto: veinte líneas y sin dependencias. A partir de aquí cada paso se puede *ver*, no solo testear |

Después de 009, por orden de utilidad: audio (fase 3), encoding real (fase 4),
captura con PipeWire (fase 5) —el momento en que el programa sirve para algo—,
streaming (fase 6) e interfaz (fase 7).

Deuda de rendimiento pendiente en `docs/PERFORMANCE.md`. La más gorda —la
conversión de color, 25 % del presupuesto— tiene su arreglo más barato dentro
del plan 008: **escalar antes de convertir**, como hace OBS, medido en 2,2×.

## Fase 2 — Compositor
- Composición escena → frame con transformaciones, recorte, mezcla alfa y
  escalado de calidad. Camino CPU verificable + tests de imagen dorada.
- Backend GPU según el ADR de render.
- **Criterio de salida:** 1080p60 compuesto dentro del presupuesto, con
  benchmark publicado.

## Fase 3 — Audio
- Mezclador multipista, medidores, resampling, sincronía A/V anclada al reloj de
  audio.
- **Criterio de salida:** mezcla sin asignaciones ni bloqueos en el callback.

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
