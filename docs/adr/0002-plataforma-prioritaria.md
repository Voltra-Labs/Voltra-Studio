# ADR 0002 — Plataforma prioritaria: Linux (PipeWire + Wayland)

- **Estado:** aceptada (2026-08-30)
- **Fase:** 5

## Contexto

La captura es lo único verdaderamente atado al sistema operativo. Hacer las tres
plataformas a la vez multiplica el trabajo antes de tener nada usable.

## Decisión

Linux es la plataforma de referencia:

| Necesidad | API |
|---|---|
| Pantalla y ventana | PipeWire + portales XDG (`org.freedesktop.portal.ScreenCast`) |
| Cámara | V4L2 (y PipeWire cuando esté disponible) |
| Audio de sistema y micrófono | PipeWire (respaldo PulseAudio) |
| Encoding hardware | VAAPI (ver ADR 0003) |

Windows y macOS **no se descartan**: `voltra-capture` define los traits
(`ScreenCapture`, `WindowCapture`, `CameraCapture`, `AudioCapture`) sin sesgo de
plataforma, y cada backend vive tras `#[cfg]` + feature. Nada específico de
Linux puede filtrarse a `voltra-core` ni a `voltra-engine`.

## Justificación

Es donde OBS peor funciona hoy (Wayland, portales, VAAPI), o sea donde más valor
aporta hacerlo bien. Y es la plataforma que el entorno de desarrollo puede
compilar de forma nativa.

## Consecuencias

- El primer producto usable es Linux-only; se comunica sin ambigüedad.
- Riesgo: diseñar los traits mirando solo a PipeWire. Mitigación: cada trait de
  captura documenta cómo lo cumpliría Windows.Graphics.Capture y
  ScreenCaptureKit antes de darse por bueno.
