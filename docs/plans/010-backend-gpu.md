# 010 — Backend GPU (`wgpu`)

- **Fase:** 2
- **Estado:** en curso

## Objetivo

Un segundo compositor, `GpuCompositor`, detrás del mismo trait `Compositor` que
el del plan 008: escena → frame, con los siete modos de mezcla y los dos filtros,
ejecutando en GPU vía `wgpu`. Validado **contra el camino CPU**, que pasa a ser
el oráculo. Cierra el compromiso del ADR 0001 y el requisito que el plan 008
convirtió en obligatorio con datos.

## Fuera de alcance

- **Camino sin copia hacia el encoder.** El ADR 0003 quiere que el frame
  compuesto llegue al codificador sin bajar a CPU. Aquí se descarga siempre,
  porque es lo que promete el trait hoy y es lo que hace posible compararlo con
  el oráculo. El trait crecerá una variante "déjalo en la GPU" cuando exista un
  encoder que la aproveche; hacerlo ahora sería diseñar contra un consumidor
  imaginario.
- **Cifras de rendimiento de GPU.** Este contenedor no tiene GPU: se verifica
  sobre **lavapipe**, el rasterizador software de Mesa. Eso demuestra
  corrección y nada más. Publicar un número de lavapipe como "rendimiento GPU"
  sería justo lo que CLAUDE.md §4.8 prohíbe. El presupuesto de la fase 2 se
  cierra cuando alguien lo mida en hardware real.
- **Pool de texturas y doble búfer de descarga.** OBS usa doble búfer para que
  la CPU no espere a la GPU. Es una optimización con su propia medición, y sin
  hardware donde medirla no se escribe.
- **Filtros de fuente encadenados, transiciones y escalado de salida.**

## Referencia

`docs/references/gpu-compositing.md`, escrito para este plan. En resumen: se
adopta el bucle de `obs_scene_video_render` —proyección ortográfica en píxeles
de lienzo, una matriz por item, blend state por item, un quad texturizado— y la
tabla de blend de la que ya salió el módulo `blend.rs` del plan 008.

**Qué mejoramos:**

1. **El camino CPU sobrevive como oráculo.** OBS no tiene respaldo CPU: sin GPU
   no hay OBS. Aquí el backend nuevo se valida contra dieciséis imágenes doradas
   que ya pasan. Es una clase de verificación que el referente no puede hacer.
2. **Un trait, dos backends.** `Compositor` no cambia, así que `voltra render`
   produce el mismo Y4M por los dos caminos y se pueden comparar frame a frame.
3. **La ausencia de GPU es un estado, no un fallo.** Sin adaptador,
   `GpuCompositor::new` devuelve `Error::Unsupported` y quien llama decide. Es
   CLAUDE.md §5: un componente puede faltar sin tumbar el resto.

## Diseño

Todo dentro de `voltra-render`, tras la feature **`gpu`**, apagada por omisión:
`cargo test --workspace` tiene que seguir corriendo en un contenedor sin GPU
(CLAUDE.md §3).

```
crates/voltra-render/src/gpu/
├── mod.rs          declara y reexporta
├── context.rs      instancia, adaptador, dispositivo, cola
├── pipeline.rs     los siete blend states y sus pipelines
├── texture.rs      caché de texturas de fuente, subida por frame
├── readback.rs     textura → VideoFrame, con el relleno de 256 bytes
├── compositor.rs   GpuCompositor: el bucle por item
└── composite.wgsl  vértice y fragmento
```

**La diferencia estructural con el camino CPU.** El compositor CPU va hacia
atrás: por cada píxel de destino invierte la matriz y busca el texel. El GPU va
hacia delante: transforma las **cuatro esquinas** del quad y deja que el
rasterizador interpole. `Placement::forward` (plan 002) ya da exactamente esa
transformación, y `Placement::src` da el rectángulo de origen tras el recorte,
así que las cuatro esquinas del quad y sus cuatro UV salen de datos que ya
existen. No hace falta geometría nueva.

Por item:

1. `transform.place(tamaño de la fuente)` → `Placement`. `None` = se salta.
   Idéntico al camino CPU, misma función.
2. Las cuatro esquinas: `forward.apply` de las esquinas de `src`. Las UV: esas
   mismas esquinas divididas por el tamaño de la textura.
3. Vértices y matriz van a un uniform; el modo de mezcla elige uno de los siete
   pipelines, ya construidos; el filtro elige uno de los dos samplers, ya
   construidos.
4. Un `draw` de cuatro vértices.

El fragment shader es `textureSample` y una multiplicación por alfa —el
premultiplicado que el `ONE` de la tabla de libobs exige—. Todo lo demás es
estado fijo.

Al final del pase: `copy_texture_to_buffer` con las filas rellenadas a múltiplo
de 256, `map_async`, y copia fila a fila al `VideoFrame` quitando el relleno.

## Pasos

1. Feature `gpu`, dependencias y `GpuContext` con adaptador ausente como estado.
2. Descarga: textura → `VideoFrame`, con el relleno de 256 bytes y su test.
3. Shader, pipelines de los siete modos y los dos samplers.
4. `GpuCompositor::composite`: el bucle por item.
5. Suite de paridad contra el oráculo CPU.
6. `voltra render --gpu`.
7. Documentación, puerta de calidad y commit.

## Verificación

La suite de paridad ejecuta la **misma escena** por los dos backends y compara.
Se salta entera, sin fallar, cuando no hay adaptador.

Donde el resultado tiene que ser **exacto**, se exige exacto:

- Lienzo vacío: el color de fondo, alfa incluido.
- Una fuente opaca 1:1 sin transformación: la fuente, bit a bit.
- Un item invisible o sin frame: no se dibuja, y los contadores lo dicen.
- Un item entero fuera del lienzo: fondo intacto.

Donde no puede serlo, se exige **tolerancia justificada**: el bilineal de GPU es
`f32` con precisión de subtexel definida por la implementación (Vulkan garantiza
al menos 8 bits fraccionarios), y el de CPU es punto fijo 16.16. No pueden
coincidir bit a bit, y fingir que sí sería ajustar el test al resultado:

- Rotación de 90°, escalado ×2 y ×0,5 con los dos filtros.
- Los siete modos de mezcla, cada uno con su caso conocido.
- Orden de capas y alfa parcial.

Además:

- Un lienzo de anchura **no** múltiplo de 64 píxeles (fila no alineada a 256
  bytes) se descarga sin inclinarse. Es el fallo clásico de la descarga.
- `voltra render --gpu` produce un Y4M válido del mismo tamaño que el camino CPU.

Criterio de aceptación: **la suite de paridad pasa sobre lavapipe**, y
`cargo test --workspace` sin features sigue verde en un contenedor sin GPU.
El criterio de rendimiento de la fase 2 **no** se cierra aquí: necesita hardware.

## Riesgos

- **Sin GPU no se puede medir.** Es el riesgo aceptado y declarado arriba. La
  mitigación es no pretender lo contrario: este plan entrega corrección, y el
  presupuesto queda abierto en `docs/PERFORMANCE.md` hasta que haya hardware.
- **Divergencia CPU/GPU en los bordes.** Un píxel de borde puede caer a un lado
  u otro de la regla de relleno del rasterizador. Mitigación: las comparaciones
  con tolerancia cuentan píxeles discrepantes además de medir su magnitud, para
  que "dos píxeles del borde por 1" no se confunda con "toda la imagen por 1".
- **`wgpu` es una dependencia grande.** Es la que fijó el ADR 0001 tras evaluar
  alternativas, va tras feature apagada por omisión, y no entra en el build por
  defecto de CI.
- **Tentación de optimizar sobre lavapipe.** Cualquier perfil sacado de un
  rasterizador software dice cosas falsas sobre hardware real. No se optimiza
  nada en este plan.
