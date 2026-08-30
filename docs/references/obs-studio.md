# Estado del arte: OBS Studio / libobs

Resumen de **cómo lo hace OBS de verdad**, contrastado con su documentación
oficial y con el análisis del código. Sirve de base de diseño: cuando Voltra se
aparte de esto, el ADR correspondiente debe decir por qué.

Fuentes: [Backend Design (docs.obsproject.com)](https://docs.obsproject.com/backend-design),
[libobs Core](https://deepwiki.com/obsproject/obs-studio/2.1-libobs-core),
[Video and Audio Pipelines](https://deepwiki.com/obsproject/obs-studio/2.4-video-and-audio-pipelines),
[Video Encoders](https://deepwiki.com/obsproject/obs-studio/4.4-video-encoders),
[Hardware Encoding (KB)](https://obsproject.com/kb/hardware-encoding),
[PR #3863 — audio mixing robusto](https://github.com/obsproject/obs-studio/pull/3863).

## 1. Objetos del núcleo

| Objeto | Papel |
|---|---|
| `obs_source_t` | Todo lo que produce o procesa A/V: entradas, **filtros** y **transiciones** son sources también. |
| `obs_scene_t` / `obs_sceneitem_t` | Una escena es una source que contiene items; el item guarda la transformación (`obs_transform_info`: pos, rot, escala, alineación, bounds) y el recorte (`obs_sceneitem_crop`). |
| `obs_output_t` | Consume A/V ya codificado (o crudo) y lo entrega: fichero, red, dispositivo. |
| `obs_encoder_t` | Comprime vídeo/audio. Se conecta al output con `obs_output_set_video_encoder()`. |
| `obs_service_t` | Parámetros de la plataforma de streaming (URL, clave). |

Ideas clave que adoptamos:
- **Un solo trait raíz para fuente/filtro/transición** simplifica enormemente
  el grafo: una escena es una fuente más, lo que da anidamiento gratis.
- **Canales de salida** (`0..MAX_CHANNELS`): la mezcla final no es "la escena
  activa", sino un conjunto de canales; la escena vive en uno y el audio global
  en otros.
- **Settings + properties**: `obs_data_t` para serializar y `obs_properties_t`
  para que la UI se genere sola desde el plugin. Es la razón de que OBS tenga
  ecosistema.

## 2. Pipeline de vídeo

`obs_graphics_thread` (en `libobs/obs-video.c`) corre a la cadencia configurada.
Cada iteración:

1. **`tick_sources()`** — actualiza todas las fuentes activas con el tiempo
   transcurrido. El tick va **separado** del render.
2. **`render_main_texture()`** — limpia el destino, fija proyección ortográfica
   del tamaño del lienzo y dibuja los canales de salida. Si varias mezclas
   comparten vista, **reutiliza la textura ya renderizada**.
3. **`render_output_texture()`** — reescalado a la resolución de salida cuando
   difiere del lienzo (efecto *bilinear lowres* si se baja a menos de la mitad).
4. **`render_convert_texture()`** — conversión RGB→YUV **en GPU**, renderizando
   cada plano por separado con la matriz de espacio de color.
5. **`stage_output_texture()`** — copia GPU→CPU con **doble búfer** y descarga
   asíncrona, para no bloquear esperando a la GPU.

Después, los frames se reparten a callbacks de vídeo crudo y a los encoders. Hay
un segundo hilo (`video_thread` en `media-io/video-io.c`) dedicado a
encoding/salida, y contadores de frames saltados/perdidos.

**Lecciones que nos llevamos:** tick separado de render; conversión de color en
GPU y no en CPU; descarga asíncrona con doble búfer; reutilización de texturas;
y el reescalado como etapa explícita del pipeline, no como efecto colateral.

## 3. Pipeline de audio

Modelo *pull*: `audio_callback()` pide mezcla a intervalos regulares (~21,3 ms a
48 kHz, bloques de 1024 muestras).

1. Las fuentes entregan con `obs_source_output_audio()`, que **remuestrea** si
   hace falta y empuja a `audio_input_buf[]` (colas circulares).
2. Cada tick se toma un **snapshot del árbol de fuentes de audio**, marcando las
   duplicadas (`audio_is_duplicated`).
3. `mix_audio()` alinea por marca de tiempo: el desfase de cada fuente respecto
   al inicio del intervalo se resuelve con `start_point`.
4. Se aplican volumen, balance/paneo y monitorización; los nodos con hijos
   (escenas, transiciones) mezclan mediante `audio_render`.

OBS **bufferiza hasta ~1 s** para alinear fuentes con relojes distintos, y una
fuente que se sale de ese margen sufre un *dropout* **sin arrastrar al resto**
(PR #3863). Existe `Sync Offset (ms)` por fuente, que admite valores negativos.

**Lecciones:** el reloj de audio manda; alineación por marca de tiempo y no por
orden de llegada; aislamiento del fallo por fuente; y desfase manual por fuente
como escape para el usuario. El coste que queremos evitar: OBS reconoce que ese
buffering "introdujo una complejidad significativa" — nosotros lo modelamos
explícitamente desde el principio.

## 4. Encoding

Los encoders son plugins que se registran con `obs_encoder_info`. NVENC se
implementa vía FFmpeg y también de forma nativa; AMF usa el SDK de AMD; QSV usa
Intel VPL; macOS usa VideoToolbox; Linux, VAAPI. x264 es el respaldo software.

Lo decisivo para el rendimiento: los encoders con
`OBS_ENCODER_CAP_PASS_TEXTURE` reciben **texturas de GPU directamente**, sin
viaje de ida y vuelta a la CPU, con camino de respaldo a memoria de sistema
cuando el compartido de texturas no está disponible.

**Lección:** la ruta rápida no es "elegir buen encoder", es **no copiar el frame
a CPU**. El diseño del pipeline tiene que permitir que el frame nunca baje de la
GPU.

## 5. Extensiones

`OBS_DECLARE_MODULE()`, `obs_module_load()/unload()` y registros
(`obs_register_source`, `obs_register_encoder`, ...). Es una **ABI de C con
punteros a función**: potente, pero un plugin defectuoso tumba el proceso
entero.

**Aquí es donde Voltra puede mejorar de verdad:** el modelo de extensiones debe
aislar el fallo (proceso separado o WASM) para que un plugin no pueda romper una
emisión en directo.

## 6. Resumen: qué adoptamos y qué mejoramos

| Adoptamos | Mejoramos |
|---|---|
| Grafo de fuentes unificado (fuente/filtro/transición/escena) | Tipado fuerte en vez de `void*` y punteros a función |
| Canales de salida y mezclas múltiples | Estado publicado como snapshot inmutable, sin bloqueos en render |
| Tick separado de render | Reloj racional en todo el pipeline (nada de `f64`) |
| Conversión de color en GPU + descarga asíncrona | Camino CPU de referencia verificado con imágenes doradas |
| Alineación de audio por marca de tiempo con aislamiento de fallos | Modelo explícito de buffering, no emergente |
| Encoding sin copia a CPU (paso de texturas) | El pipeline exige el camino sin copia desde el diseño |
| Properties que generan la UI | Sin ABI de C frágil: extensiones aisladas |
