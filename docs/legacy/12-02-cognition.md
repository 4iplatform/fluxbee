Thread (hilo) es la unidad física y concreta. Lo crea el nodo IO con un hash determinístico basado en el grupo de ICHs + ILKs que participan. Es el equivalente a "este grupo de WhatsApp", "este canal de Slack", "esta conversación de email". El thread_id es estable: mismos participantes en mismo canal = mismo thread. El IO es dueño de crear y resolver esto porque es el que conoce el mundo exterior.
Scope es la unidad abstracta de continuidad. Agrupa threads que comparten historia. Sirve para que cuando se abre un thread nuevo, se le pueda cargar contexto previo (contextos, memorias, episodios) de threads anteriores relacionados. La asociación inicial de un thread a un scope existente queda por definir bien, pero la idea es por coincidencia de interlocutores o canales.
Dentro de un thread viven: mensajes (con tags), contextos (temas semánticos con peso), memorias (extractos resumidos con decay) y episodios (eventos socio-emocionales con gate). Todo esto tiene hash propio + thread_id + scope_id (si ya existe).
El flujo real es:

IO recibe mensaje → le pone thread_id (hash) → lo manda por router
Router retransmite al nodo AI destino Y al cognitive (en paralelo)
AI procesa con lo que tenga (al principio solo el mensaje)
Cognitive: tagger → context evaluator → actualiza pesos → evalúa memorias/episodios
Cognitive guarda en local (LanceDB/memoria para threads activos)
Cuando hay cambios relevantes (scope creado, contextos maduros, memorias, episodios), fire & forget por NATS
En mensajes siguientes del mismo thread, cognitive ya tiene contexto acumulado que enriquece lo que viaja con el mensaje
Sobre el scope: no aparece inmediato. Con un solo mensaje no hay scope. El scope emerge cuando hay suficiente contexto acumulado en el thread, y su valor real es cuando se puede asociar a otro thread para transferir historia.