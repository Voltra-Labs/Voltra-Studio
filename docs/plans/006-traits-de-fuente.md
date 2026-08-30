# 006 — Traits de fuente y primera fuente real

- **Fase:** 1
- **Estado:** propuesto

## Objetivo

Definir el contrato que cumple todo lo que produce o procesa imagen: `Source`,
`VideoSource` y `VideoFilter`, con identidad, capacidades declaradas y contexto
de tick. Y demostrarlo con una implementación real —una fuente de color sólido y
un filtro de opacidad— para que los traits no sean una abstracción sin usuario.

## Fuera de alcance

- **Traits de audio.** No existen todavía los tipos de audio (`AudioBuffer`,
  disposición de canales); un `AudioSource` sin ellos sería humo. Fase 3.
- **El modelo de render en GPU.** Ver la nota de diseño: necesita el backend
  gráfico y por tanto no puede vivir en `voltra-core`.
- **Registro de fuentes por nombre y sistema de propiedades** (`obs_data` /
  `obs_properties`). Hace falta cuando haya configuración que cargar: plan 008.
- Grafo de escena y transiciones: plan 007.

## Referencia

De `libobs/obs-source.h`, leído directamente:

```c
enum obs_source_type { OBS_SOURCE_TYPE_INPUT, OBS_SOURCE_TYPE_FILTER,
                       OBS_SOURCE_TYPE_TRANSITION, OBS_SOURCE_TYPE_SCENE };

#define OBS_SOURCE_VIDEO      (1 << 0)
#define OBS_SOURCE_AUDIO      (1 << 1)
#define OBS_SOURCE_ASYNC      (1 << 2)
#define OBS_SOURCE_COMPOSITE  (1 << 6)
```

`struct obs_source_info` declara `id`, `type`, `output_flags` y los callbacks:
`create`/`destroy`, `get_width`/`get_height`, `video_tick`, `video_render`,
`filter_video`, `update`, `activate`/`deactivate`, `show`/`hide`,
`enum_active_sources`, `save`/`load`.

Lo decisivo son **los dos modelos de entrega de vídeo**:

- **Asíncrono** (`OBS_SOURCE_ASYNC_VIDEO`): la fuente empuja frames de memoria
  con `obs_source_output_video`. Es el modelo de cámaras y ficheros, que no
  controlan cuándo llega cada frame.
- **De render** (`video_render`): la fuente se dibuja con la API gráfica cuando
  el compositor se lo pide. Es el modelo de la captura de pantalla y del texto.

**Qué copiamos:** los cuatro tipos de fuente; el que **filtros, transiciones y
escenas sean fuentes también**, que es lo que da anidamiento gratis; la
separación tick/render; y los dos modelos de entrega.

**Qué mejoramos:**

1. `output_flags` es un `u32` donde nada impide declarar disparates —
   `OBS_SOURCE_ASYNC` sin `OBS_SOURCE_VIDEO`, por ejemplo. Aquí las capacidades
   son un tipo con constructores nombrados: las combinaciones imposibles no se
   pueden escribir.
2. `video_tick` recibe `float seconds` acumulados desde el frame anterior. Un
   `f32` acumulado es exactamente la deriva que el plan 002 se ocupó de eliminar.
   Nuestro `TickContext` lleva el **pts exacto** y la cadencia racional; el delta
   se ofrece por comodidad, pero la fuente que quiera precisión tiene la verdad.
3. `create` devuelve `void*` y todo lo demás lo recibe de vuelta sin tipo. Aquí
   una fuente es un objeto con sus datos; no hay punteros opacos que castear.
4. Un fallo de fuente devuelve `Result` en vez de escribirse en un log y seguir.
   Es lo que permite la regla de CLAUDE.md §5: una fuente rota se degrada, no
   tumba la emisión.

### Nota de diseño: por qué aquí solo está el modelo asíncrono

`video_render` recibe un `gs_effect_t*`: es un callback del backend gráfico.
Meterlo en `voltra-core` obligaría a que el vocabulario común conociera la GPU,
que es justo lo que la regla de estructura prohíbe.

Así que `voltra-core` define el modelo asíncrono —la fuente produce un
`VideoFrame` de CPU— y el modelo de render será un trait aparte en
`voltra-render`, junto al backend. Una fuente podrá implementar uno, el otro o
los dos, igual que en OBS.

## Diseño

### `voltra-core::source`

```rust
pub struct SourceId(u64);          // identidad estable, no un puntero
pub enum SourceType { Input, Filter, Transition, Scene }

pub struct SourceCapabilities { video: bool, audio: bool, composite: bool, … }

pub struct TickContext { pts: Timestamp, frame: u64, fps: Fps }

pub trait Source {
    fn kind(&self) -> &'static str;
    fn source_type(&self) -> SourceType;
    fn capabilities(&self) -> SourceCapabilities;
    fn tick(&mut self, ctx: &TickContext) -> Result<()> { Ok(()) }
    fn activate(&mut self) {}      // entra en el programa
    fn deactivate(&mut self) {}
}

pub trait VideoSource: Source {
    fn size(&self) -> Option<FrameSize>;      // None hasta el primer frame
    fn frame(&self) -> Option<&VideoFrame>;
}

pub trait VideoFilter: Source {
    fn filter(&mut self, frame: &mut VideoFrame) -> Result<()>;
}
```

`VideoFilter::filter` trabaja **en el sitio**, como `filter_video` en OBS, que
recibe y devuelve el mismo frame. Un filtro que necesite un segundo búfer se
lleva su propio `FramePool`: el coste queda donde se decide, no impuesto a todos.

`size()` devuelve `Option` porque una fuente asíncrona no sabe su tamaño hasta
que llega el primer frame — exactamente el motivo por el que `get_width` es
opcional para fuentes asíncronas en libobs.

### `voltra-sources` (crate nueva)

Primeras inquilinas, sin dependencias del sistema:

- `ColorSource`: un color sólido de tamaño fijo. Es el equivalente de la fuente
  «Color» de OBS y el material de prueba del compositor.
- `OpacityFilter`: multiplica el canal alfa. Es el filtro más simple que hace
  algo real, y es la pieza de las transiciones de fundido.

Se añade a CLAUDE.md §2: crate de fuentes integradas, depende solo de
`voltra-core`.

## Pasos

1. `voltra-core/src/source.rs`: identidad, tipos, capacidades, `TickContext` y
   los tres traits.
2. Crate `voltra-sources` con `ColorSource`.
3. `OpacityFilter` en la misma crate.
4. Actualizar CLAUDE.md §2 con la crate nueva.
5. Puerta de calidad.

## Verificación

- `SourceId` es monótono y único desde su generador.
- Las capacidades no admiten combinaciones imposibles (constructores nombrados).
- `TickContext` da el pts exacto del índice de frame pedido, sin acumular.
- `ColorSource` produce un frame del color y tamaño pedidos, con alfa correcto,
  y **reutiliza su búfer entre ticks** (se compara el puntero: no puede asignar
  por frame).
- Cambiar el color de `ColorSource` se refleja en el siguiente tick, no en el
  actual: el tick es el punto de sincronía.
- `OpacityFilter` a 1,0 no cambia nada; a 0,0 deja el alfa a cero; a 0,5 lo
  reduce a la mitad; y **no toca los canales de color**.
- Un filtro sobre un formato sin alfa devuelve `Error::Unsupported`, no basura.
- Los traits son objeto-seguros: `Box<dyn VideoSource>` compila.

## Riesgos

- **Diseñar los traits mirando solo a fuentes sintéticas.** Una cámara real tiene
  latencia, formatos cambiantes y fallos intermitentes. Mitigación: cada método
  documenta cómo lo cumpliría una captura de PipeWire, que es la plataforma
  prioritaria del ADR 0002.
- **`&mut self` en `tick` frente a acceso concurrente.** Se asume un dueño único
  por fuente, coherente con el modelo de hilos previsto. Si la captura resulta
  necesitar entrega desde otro hilo, será un canal hacia el dueño, no un
  `Mutex` compartido.
