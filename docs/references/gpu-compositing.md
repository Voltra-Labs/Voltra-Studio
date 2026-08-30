# Estado del arte: composición en GPU

Referencia para el plan 010, el backend acelerado que el ADR 0001 dejó
comprometido y que el plan 008 convirtió en obligatorio con datos.

---

## 1. Cómo compone libobs

Fuente: `libobs/obs-scene.c`, `libobs/obs-video.c` y `libobs/graphics/`, resumido
en `docs/references/obs-studio.md` §2.

OBS **no tiene camino CPU**. Toda la composición es GPU, y el bucle por item de
`obs_scene_video_render` es:

1. **Proyección ortográfica del tamaño del lienzo**
   (`gs_ortho(0, width, 0, height, -100, 100)`), de modo que el shader trabaja
   directamente en píxeles de lienzo y no en coordenadas normalizadas.
2. **Una matriz por item** (`gs_matrix_mul` con la transformación del
   `obs_sceneitem_t`), empujada y sacada de una pila.
3. **Blend state por item**, de una tabla fija
   `(src_color, src_alpha, dst_color, dst_alpha, op)`.
4. **Un quad texturizado** dibujado con el efecto `default.effect`, que es
   literalmente `texture.Sample(sampler, uv)`.
5. Al final, `stage_output_texture()` descarga el resultado a memoria de sistema
   con doble búfer para que la CPU no espere a la GPU.

Dos cosas que conviene subrayar porque marcan el diseño:

- **La transformación va hacia delante.** OBS transforma las *cuatro esquinas*
  del quad y deja que el rasterizador interpole; no hay mapeo inverso por píxel.
  El camino CPU del plan 008 hace lo contrario —por cada píxel de destino,
  invierte la matriz para saber qué texel leer— porque en CPU no hay
  rasterizador. Es la diferencia estructural entre los dos caminos.
- **El filtrado es del sampler, no del programa.** `Point` y `Bilinear` son
  estados del muestreador. En CPU costaban 110 ms por frame antes de
  optimizarlos (plan 008); en GPU son un `enum` de dos valores.

## 2. La tabla de blend, y por qué ya la tenemos

El módulo `blend.rs` del plan 008 **se escribió leyendo esa tabla**: cada modo
CPU es la forma desarrollada de una ecuación de blend state de libobs. El
backend GPU no traduce nada, vuelve a la forma original.

| Modo | libobs | `wgpu::BlendComponent` |
|---|---|---|
| `Normal` | `ONE, INVSRCALPHA, ADD` | `One`, `OneMinusSrcAlpha`, `Add` |
| `Additive` | `ONE, ONE, ADD` | `One`, `One`, `Add` |
| `Subtract` | `ONE, ONE, REVERSE_SUBTRACT` | `One`, `One`, `ReverseSubtract` |
| `Screen` | `ONE, INVSRCCOLOR, ADD` | `One`, `OneMinusSrc`, `Add` |
| `Multiply` | `DSTCOLOR, INVSRCALPHA, ADD` | `Dst`, `OneMinusSrcAlpha`, `Add` |
| `Lighten` | `ONE, ONE, MAX` | `One`, `One`, `Max` |
| `Darken` | `ONE, ONE, MIN` | `One`, `One`, `Min` |

Que las dos implementaciones deriven de la misma tabla es lo que hace honesto
compararlas: no es una reimplementación libre que "parece dar lo mismo", es la
misma ecuación evaluada por hardware distinto.

**Alfa premultiplicado.** El `ONE` en el color de origen de `Normal` solo tiene
sentido si el origen llega premultiplicado. En CPU lo hace `premultiply()` por
píxel; en GPU lo hace el fragment shader una vez por fragmento. Misma cuenta.

## 3. Qué aporta `wgpu`

`wgpu` es la implementación en Rust de WebGPU y el backend que fijó el ADR 0001.
Traduce a Vulkan, Metal, D3D12 y GL, que es exactamente el reparto de plataformas
que Voltra necesita sin escribir cuatro backends.

Lo que importa para este plan:

- **Funciona sin pantalla.** No hace falta ventana ni superficie: se renderiza a
  una textura y se lee de vuelta. Es lo que permite que el backend se pueda
  testear en CI.
- **Funciona sin GPU.** Con [lavapipe](https://docs.mesa3d.org/drivers/llvmpipe.html)
  —el rasterizador software de Mesa, paquete `mesa-vulkan-drivers`— `wgpu`
  enumera un adaptador `DeviceType::Cpu` y ejecuta el mismo código. **Verifica
  corrección, no rendimiento:** es una CPU haciendo de GPU.
- **La descarga tiene una regla que muerde.** `copy_texture_to_buffer` exige que
  `bytes_per_row` sea múltiplo de `COPY_BYTES_PER_ROW_ALIGNMENT` (256). Una fila
  BGRA de 1920 px son 7680 bytes, que es múltiplo de 256 por casualidad; una de
  100 px son 400, que no. Si no se rellena y se quita el relleno al copiar de
  vuelta, la imagen sale inclinada. Es el mismo error de stride que el plan 009
  evitó al escribir Y4M, en el otro sentido.

## 4. Resumen: qué adoptamos y qué mejoramos

**Qué adoptamos:** el orden del bucle, la proyección ortográfica en píxeles de
lienzo, la tabla de blend, el alfa premultiplicado y el quad por item. Es el
diseño de OBS porque está bien.

**Qué hacemos distinto:**

1. **El camino CPU sigue vivo como oráculo.** OBS no tiene respaldo: sin GPU no
   hay OBS. Aquí el backend acelerado se valida **contra** el camino CPU, que ya
   pasa dieciséis imágenes doradas. Un backend nuevo que reproduce un oráculo
   existente es una clase de verificación que OBS no puede hacer.
2. **Un solo trait, dos backends.** `Compositor` no cambia (ADR 0001), así que
   `voltra render` funciona igual con `--gpu` que sin él y las dos salidas se
   pueden comparar frame a frame.
3. **El backend no puede recursionar**, por la misma razón que el CPU: resuelve
   frames por `FrameProvider`, nunca fuentes.
