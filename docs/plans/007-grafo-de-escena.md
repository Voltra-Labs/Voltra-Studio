# 007 — Grafo de escena

- **Fase:** 1 → 2
- **Estado:** propuesto

## Objetivo

`Scene` y `SceneItem`: una lista ordenada de fuentes colocadas sobre un lienzo,
con visibilidad, bloqueo, modo de mezcla y filtro de escalado, más las
operaciones de reordenación que la interfaz necesitará. Es lo que el compositor
del plan 008 va a recorrer una vez por frame.

## Fuera de alcance

- **Componer.** Este paso describe la escena; dibujarla es el plan 008.
- **Detección de ciclos** (una escena que se contiene a sí misma). Requiere
  resolver `SourceId` → fuente, y eso es el registro de fuentes, que todavía no
  existe. Se deja el ladrillo (`Scene::references`) y se documenta el agujero.
- **Transiciones de mostrar/ocultar por item.** OBS guarda `show_transition` y
  `hide_transition` en cada item; llegan con el plan de transiciones.
- Grupos. En OBS un grupo es una escena marcada `is_group`; se añadirá cuando
  haya interfaz que lo justifique.

## Referencia

De `libobs/obs-scene.h`, leído directamente:

```c
struct obs_scene_item {
    int64_t id;
    struct obs_scene *parent;  struct obs_source *source;
    bool user_visible;  bool visible;  bool selected;  bool locked;
    struct obs_sceneitem_crop crop;
    struct vec2 pos;  struct vec2 scale;  float rot;  uint32_t align;
    enum obs_bounds_type bounds_type;  uint32_t bounds_align;  struct vec2 bounds;
    enum obs_scale_type scale_filter;
    enum obs_blending_method blend_method;  enum obs_blending_type blend_type;
    /* would do **prev_next, but not really great for reordering */
    struct obs_scene_item *prev;  struct obs_scene_item *next;
};

struct obs_scene {
    struct obs_source *source;  bool is_group;  bool custom_size;
    uint32_t cx, cy;  int64_t id_counter;
    pthread_mutex_t video_mutex;  pthread_mutex_t audio_mutex;
    struct obs_scene_item *first_item;
};
```

Modos de mezcla (`obs_blending_type`): `NORMAL`, `ADDITIVE`, `SUBTRACT`,
`SCREEN`, `MULTIPLY`, `LIGHTEN`, `DARKEN`.
Filtros de escalado (`obs_scale_type`): `DISABLE`, `POINT`, `BICUBIC`,
`BILINEAR`, `LANCZOS`, `AREA`.

**Qué copiamos:** el modelo entero de item —identificador propio, fuente,
visibilidad, bloqueo, transformación, mezcla y filtro de escalado—, el contador
de identificadores por escena, y los siete modos de mezcla con los seis filtros.

**Qué mejoramos:**

1. **Vector en vez de lista enlazada.** El propio código de OBS lleva el
   comentario *"would do \*\*prev_next, but not really great for reordering"*: la
   lista enlazada les incomoda y la mantienen igual. El compositor recorre esta
   lista **una vez por frame**, y perseguir punteros por el montón es justo lo
   que prohíbe CLAUDE.md §4.5. Un `Vec` contiguo es mejor para la caché y
   convierte la reordenación en un `swap`. Una escena tiene decenas de items, no
   millones: no hay coste de inserción que compense.
2. **El orden de dibujo se declara.** En OBS, que la lista vaya de arriba abajo o
   al revés es folclore que se aprende leyendo el render. Aquí `items()` va
   explícitamente **de atrás hacia delante**, en el orden en que se pinta, y está
   escrito en la documentación del método y fijado por un test.
3. **El identificador de item sobrevive a la reordenación.** En OBS el `id` es
   estable pero el orden vive en los punteros; aquí son dos cosas separadas por
   construcción, y hay un test que lo comprueba. Es lo que permite que la
   interfaz guarde una selección mientras el usuario arrastra capas.
4. Sin `mutex` en la escena. La escena se edita en una copia y se publica como
   snapshot (CLAUDE.md §5); no es una estructura compartida con cerrojo.

## Diseño

```rust
pub struct SceneItemId(u64);          // único dentro de su escena

pub struct SceneItem {
    id: SceneItemId,
    source: SourceId,
    transform: Transform,             // del plan 002: recorte, escala, bounds…
    visible: bool,
    locked: bool,
    blend: BlendMode,
    scale_filter: ScaleFilter,
}

pub struct Scene {
    name: String,
    size: FrameSize,                  // el lienzo
    items: Vec<SceneItem>,            // de atrás hacia delante
    next_item_id: u64,
}
```

Operaciones: `add`, `remove`, `item`/`item_mut`, `items`, `visible_items`,
`raise`, `lower`, `raise_to_top`, `lower_to_bottom`, `references`.

`visible_items()` es la entrada del compositor: filtrar una vez al recorrer sale
más barato que preguntar por item dentro del bucle de dibujo.

## Pasos

1. `voltra-core/src/scene.rs`: `SceneItemId`, `BlendMode`, `ScaleFilter`,
   `SceneItem`, `Scene` y sus operaciones.
2. Reexportar en `lib.rs`.
3. Puerta de calidad.

## Verificación

- Los identificadores de item son únicos y crecientes dentro de la escena, y no
  se reciclan al borrar.
- `items()` devuelve el orden de dibujo, de atrás hacia delante; el último
  añadido queda encima.
- `raise`/`lower` mueven un puesto y **no hacen nada en los extremos**, en vez
  de envolver o fallar.
- `raise_to_top`/`lower_to_bottom` conservan el orden relativo de los demás.
- El identificador de un item no cambia al reordenar.
- `visible_items` salta los ocultos y mantiene el orden.
- Borrar un item que no existe devuelve `None`, no un pánico.
- `references` encuentra una fuente usada por varios items.
- Modo de mezcla por defecto: `Normal`. Filtro por defecto: `Bilinear`.

Sin benchmark: no hay bucle por píxel aquí. El coste de recorrer la escena se
medirá en el plan 008, donde por fin significa algo.

## Riesgos

- **Ciclos entre escenas.** Una escena que se contiene a sí misma es recursión
  infinita en el compositor. Queda fuera de alcance por falta de registro, así
  que el plan 008 **no puede** resolver `SourceId` a ciegas: o llega antes el
  registro con su comprobación, o el compositor lleva un límite de profundidad.
  Anotado aquí para que no se olvide.
- **Bloqueo (`locked`) es solo un dato.** Impedir de verdad que la interfaz mueva
  un item bloqueado es responsabilidad de la interfaz; el tipo no lo puede
  imponer sin volverse incómodo para el resto.
