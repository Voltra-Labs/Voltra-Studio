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

**La secuencia 007–009 está cerrada.** El objetivo era llegar al primer hito
visible en tres pasos, y se llegó.

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

### Qué viene ahora

La fase 2 **no está cerrada**: su criterio de salida es 1080p60 dentro del
presupuesto, y el plan 008 midió que el camino CPU está 8× por encima
(`docs/PERFORMANCE.md` §3.4). El paso que cierra la fase es el **backend `wgpu`
del ADR 0001**, detrás del mismo trait `Compositor` y validado contra las
imágenes doradas que el camino CPU ya produce. Es el siguiente plan por escribir.

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
