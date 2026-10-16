# Visor JSON

Visor de JSON de escritorio, ligero y rápido, escrito en Rust con [egui](https://github.com/emilk/egui).
Pensado para revisar documentos grandes, compararlos y quedarse solo con lo que importa.

## Características

### Abrir documentos
- Abre uno o varios archivos (Ctrl+O), arrástralos a la ventana o pásalos por línea de comandos.
- Pega un JSON con Ctrl+V fuera de un campo de texto y se abre en una pestaña nueva.
- Formato tolerante (JSONC): acepta comentarios `//` y `/* */`, comas finales y BOM; lee UTF-8 y UTF-16.
- Conserva el orden de las claves, las claves duplicadas y el texto original de los números (`15.000000` se ve tal cual).
- Lista de archivos recientes y recarga del archivo con F5.

### Importar tabla de SQL Server
Convierte el resultado de una consulta de SQL Server Management Studio en JSON (Ctrl+I o el botón **Importar
tabla de SQL Server…**).

- En SSMS: clic derecho sobre los resultados → **Copy with Headers** (Ctrl+Mayús+C) y pégalo en la ventana.
- Cada fila se convierte en un objeto y la tabla en un arreglo; se abre en una pestaña nueva («Tabla 1», …).
- Opciones: primera fila con encabezados, `NULL` → `null`, detectar números (`34.800000` y `1218` sin comillas;
  `001` o `12:18PM` quedan como texto) y una sola fila como objeto en lugar de arreglo.
- Columnas repetidas o sin nombre se renombran (`Code`, `Code_2`, `Columna3`); los valores con saltos de línea se
  vuelven a unir en su fila.
- Pegar con Ctrl+V una tabla copiada de SSMS fuera de un campo de texto abre directamente esta ventana.

### Pestañas
- Varios documentos abiertos a la vez, cada uno en su pestaña.
- Reordenables arrastrándolas.
- Nombre editable con doble clic, F2 o clic derecho → «Cambiar nombre…». Ese nombre se propone al guardar.
- Clic derecho: cambiar nombre, copiar la ruta del archivo, **Comparar con…** (otra pestaña u otro archivo), cerrar,
  cerrar las demás.
- Se cierran con la ×, con clic medio o con Ctrl+W. Un punto (•) indica cambios en la fuente sin aplicar y ⚠ un JSON con errores.
- Además de las pestañas de documento hay pestañas de **tabla** y de **comparación** (ver abajo); se abren junto a
  la pestaña desde la que se crearon y puede haber tantas como se quiera.

### Vistas
| Vista | Atajo | Para qué sirve |
|---|---|---|
| **Árbol** | Ctrl+1 | Plegar, expandir o compactar en una línea cualquier nodo. Modo *Auto*: compacta todo lo que quepa en un ancho dado. Menú de niveles, guías de sangría, números de línea y conteo de elementos en nodos cerrados. |
| **Texto** | Ctrl+2 | El JSON formateado con sangría de 2, 4 o tabulador; copiar todo o guardar. |
| **Fuente** | Ctrl+3 | Editor del texto original con números de línea y validación en vivo, *Formatear* y *Minificar*. Si hay un error, indica línea y columna y lleva hasta él. |

### Ver un arreglo como tabla
Para revisar las partidas de un arreglo como si fueran una hoja de cálculo:

- En el árbol, clic derecho sobre el arreglo → **Ver como tabla**, o sobre uno de sus elementos → «Ver como tabla:
  todo el arreglo». También con la tecla **G**.
- Para ver solo algunos elementos: selecciónalos con **Ctrl+clic** (uno por uno) o **Mayús+clic** (un rango) y clic
  derecho → «Ver como tabla: N elementos seleccionados».
- Se abre una pestaña nueva (`archivo.json · Partidas`, o `… (3 de 20)` si es una selección). Las claves de todos los
  elementos, en orden de aparición, son las columnas; cada elemento es una fila. La columna **#** es el índice del
  elemento en el arreglo original.
- Si un elemento no tiene alguna clave, la celda se muestra con un guion tenue (—) y un fondo distinto, con la
  explicación al pasar el cursor; así no se confunde con `null` ni con una cadena vacía.
- Al pasar el cursor por una celda se ve el valor completo formateado. Clic derecho en una fila: copiar la celda, el
  elemento (formateado, en una línea o minificado), su ruta, o **Seleccionar para comparar** / **Comparar con…**.
- El botón de copiar de la barra copia la tabla separada por tabuladores, lista para pegar en Excel.
- Los botones Árbol / Texto / Fuente muestran la misma copia de los elementos como documento; el botón **Tabla**
  vuelve a la tabla. El filtro por claves deja solo esas columnas (y los elementos que las tienen).

### Búsqueda
- Busca en claves y valores (Ctrl+F) y muestra cuántas coincidencias hay.
- F3 / Mayús+F3 para ir a la siguiente o la anterior; en el árbol se abre el camino hasta el resultado y en una tabla
  se selecciona la fila.
- La barra de estado muestra la ruta del nodo seleccionado (`$.Encabezado.Partidas[3].Codigo`) y su línea en el
  archivo; con clic se copia la ruta.

### Filtro por claves
Muestra solo ciertas claves y el camino más corto desde la raíz hasta ellas. Por ejemplo, al filtrar por
`U_SO1_AUTORIZADO`:

```json
{ "Encabezado": { "U_SO1_AUTORIZADO": "N" } }
```

- Lista de claves en el panel «Filtro por claves»: una por fila, con el número de coincidencias de cada una.
- No distingue mayúsculas y minúsculas. Cada clave coincidente se muestra con todo su valor.
- En arreglos solo quedan los elementos que contienen alguna de las claves.
- Clic derecho en un nodo → «Filtrar por esta clave» la agrega a la lista.
- Se aplica al árbol, al texto, a las tablas y a las comparaciones. Copiar y guardar usan el resultado filtrado.
- La lista de claves se guarda entre sesiones.

### Comparar dos JSON
Al estilo de las extensiones de comparación de VS Code:

- Clic derecho en una pestaña → **Comparar con…** y se elige otra pestaña abierta o **Otro archivo…** (se abre y se
  compara). Se crea una pestaña `archivo1.json ↔ archivo2.json` con la primera a la izquierda (el botón ⇄
  intercambia los lados).
- Líneas alineadas: en rojo lo que solo está a la izquierda, en verde lo de la derecha y, en las líneas modificadas, los caracteres exactos que cambiaron.
- **Ignorar orden de claves** para que `{"a":1,"b":2}` y `{"b":2,"a":1}` cuenten como iguales.
- **Solo diferencias** oculta las líneas iguales y deja contexto alrededor de cada cambio.
- **Lista de cambios** por ruta JSON (`~` modificado, `+` añadido, `−` eliminado); clic para saltar a esa línea.
- Navegación con los botones de diferencia anterior / siguiente o F7 / Mayús+F7, y una regla lateral con la posición de todas las diferencias.
- Desplazamiento horizontal sincronizado (Mayús + rueda o barra inferior).
- Si el filtro por claves está activo, solo se comparan esas claves; el aviso de arriba muestra las coincidencias y
  permite quitarlo.
- Cada lado es una copia del documento en el momento de comparar (la pestaña lo recuerda al pasar el cursor). Puede
  haber varias comparaciones abiertas a la vez.

### Comparar dos elementos de un arreglo
Como el *Select for Compare* / *Compare with Selected* de VS Code, pero con nodos del JSON:

- En el árbol o en una tabla, clic derecho en un elemento → **Seleccionar para comparar**. Puede ser cualquier nodo,
  de esta pestaña o de otra. El elemento marcado queda resaltado con una franja y la etiqueta «para comparar».
- Clic derecho en el otro elemento → **Comparar con «…»**. Se abre una pestaña
  `archivo1.json línea 12 ↔ archivo1.json línea 13` (la línea donde empieza cada elemento en su archivo; si el archivo
  está en una sola línea se usa la ruta) junto a la actual, con el mismo diff lado a lado y los mismos encabezados
  completos (archivo · ruta · línea).
- Cada lado es una copia del elemento en ese momento; si el documento se vuelve a analizar o cambia el filtro, hay que
  volver a marcar el elemento.

### Copiar y guardar
- Menú contextual de cada nodo: copiar el valor formateado, en una línea o minificado, el texto sin comillas, la ruta o la clave.
- Guardar formateado o minificado; el nombre sugerido es el de la pestaña.

### Ajustes
Tema oscuro o claro, tamaño de letra, escala de la interfaz, sangría, cómo se pliega el árbol al abrir un
documento (Auto, por nivel, expandir todo), comas, comillas en claves, índices de arreglo y más. Los ajustes se guardan
automáticamente.

### Actualizaciones
Desde la versión 1.4.0 el programa avisa cuando hay una versión nueva publicada en
[GitHub](https://github.com/iscalexsoto/jsonviewer/releases) y puede instalarla él mismo:

- Al iniciar consulta la última release (como mucho una vez al día y sin bloquear la ventana). Si hay una versión
  más nueva, la barra de estado muestra «Versión X.Y.Z disponible» con los botones **Actualizar** y **Ver notas**.
  También se puede buscar a mano en el menú ☰ → **Buscar actualizaciones…**.
- **Actualizar** descarga el `jsonviewer-X.Y.Z-x64.exe` de la release en la misma carpeta que el ejecutable, comprueba
  su SHA-256 (el que publica GitHub para cada archivo o el del asset `.sha256` de la release; si no hay ninguno lo
  indica en el tooltip), renombra el ejecutable actual a `jsonviewer.old.exe` y pone el nuevo en su lugar. Después
  pregunta si **Reiniciar ahora** (abre la versión nueva con los mismos archivos) o **Más tarde**. El `.old.exe` se
  borra en el siguiente arranque.
- Si no se puede reemplazar el ejecutable (carpeta de solo lectura, permisos), el nuevo queda descargado junto al
  actual y la barra de estado dice dónde, para copiarlo a mano.
- La comprobación automática se desactiva en Ajustes → **Buscar actualizaciones al iniciar**. No se envía ningún
  dato aparte de la consulta a la API pública de GitHub.

## Atajos de teclado

| Atajo | Acción |
|---|---|
| Ctrl+O | Abrir archivo(s) |
| Ctrl+V | Pegar JSON como documento nuevo |
| Ctrl+N | Documento nuevo para escribir |
| Ctrl+I | Importar tabla de SQL Server |
| Ctrl+W / clic medio | Cerrar pestaña |
| Ctrl+Tab | Siguiente pestaña |
| F2 / doble clic en pestaña | Cambiar el nombre de la pestaña |
| F5 | Recargar archivo |
| Ctrl+1 … 3 | Árbol / Texto / Fuente |
| Ctrl+F, F3, Mayús+F3 | Buscar, siguiente, anterior |
| F7, Mayús+F7 | Comparación: diferencia siguiente / anterior |
| Ctrl+Enter | Aplicar cambios de la fuente |
| ↑ ↓ RePág AvPág Inicio Fin | Moverse (árbol y tabla) |
| ← → | Cerrar / expandir, ir al padre / primer hijo |
| Enter, Espacio, doble clic | Expandir / cerrar |
| Ctrl+clic, Mayús+clic | Seleccionar varios elementos (para verlos como tabla) |
| C | Compactar nodo en una línea |
| H | Compactar hijos (cada hijo en una línea) |
| X | Cerrar hijos |
| E | Expandir todo el subárbol |
| A | Auto-compactar el subárbol |
| G | Ver el arreglo (o los elementos seleccionados) como tabla |
| Ctrl+C | Copiar el valor seleccionado |
| Ctrl / Alt / Mayús + clic en ▶ | Compactar / compactar hijos / expandir subárbol |

La lista completa está en el menú ☰ de la barra → **Atajos de teclado**.

## Compilar y ejecutar

Requiere [Rust](https://www.rust-lang.org/tools/install) (edición 2024).

```bash
cargo run --release -- ejemplos/caso.jsonc
```

El ejecutable queda en `target/release/jsonviewer.exe`. El ícono de `assets/icon.ico` se incrusta en el .exe al
compilar (`build.rs` con `winresource`; necesita `windres` del toolchain GNU o `rc.exe` del SDK de Windows).
Se le pueden pasar uno o varios archivos como argumentos;
también puede asociarse a la extensión `.json` en Windows para abrirlos con doble clic.

En Windows usa las fuentes Consolas y Segoe UI del sistema si están disponibles.
