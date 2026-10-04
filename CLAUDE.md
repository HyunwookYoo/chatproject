# CLAUDE.md — ChatProject

## 메모리 규칙 (agentmemory)

이 프로젝트의 작업 기억은 agentmemory에 저장/로드한다. 사용자가 `/recall`, `/remember`를
직접 치지 않아도 되도록 에이전트가 알아서 한다.

### 저장

`memory_save` 호출 시 **본문 첫머리에 `[ChatProject]`를 붙이고, concepts에도 `ChatProject`를 넣는다.**

```
content:  "[ChatProject] <주제>: <내용>"
concepts: "ChatProject, <구체 키워드>, ..."
type:     pattern | preference | architecture | bug | workflow | fact
```

예:

```
content:  "[ChatProject] 채팅 서버 구조: WebSocket 게이트웨이를 분리하고 방 상태는 Redis에 둔다. 이유는 ..."
concepts: "ChatProject, websocket, redis, architecture"
type:     architecture
```

`project` 인자는 넘기지 않는다 — 받기만 하고 저장되지 않는다 (근거는 아래).

전역 `~/.claude/CLAUDE.md`는 `[... / <project>]` 형식을 규정하지만, **이 프로젝트에서는 위의
flat 형식(`[ChatProject] <주제>`)이 우선한다.** 기존 저장소에서 flat 형식이 다수(233건 중 200건)이고,
recall compact 출력이 title만 보여주므로 프로젝트명이 항상 같은 위치·같은 폭으로 오는 편이 스캔하기 쉽다.

### 로드

`memory_recall` / `memory_smart_search` 쿼리에 **항상 `ChatProject`를 포함**한다.

```
memory_recall(query: "ChatProject <주제 키워드>")
```

- 새 작업이나 주제를 시작할 때 최소 1회. 세션의 첫 실질 턴에서는 반드시.
- 같은 주제를 한 세션 안에서 반복 recall하지 않는다 (비용/노이즈).
- 사소한 대화 턴에서는 생략한다.

### 왜 prefix인가 — 2026-08-23 실측 (agentmemory 0.9.27)

- `memory_save`의 `project` 인자는 **받기만 하고 저장하지 않는다.** 저장된 메모리 234개 중
  `project` 필드를 가진 레코드는 0개.
- 반면 **세션**은 `project`를 가진다. 훅의 `resolveProject()`가
  `AGENTMEMORY_PROJECT_NAME` → git toplevel basename → cwd basename 순으로 결정한다.
  이 프로젝트는 git repo가 아니므로 cwd basename인 `ChatProject`가 자동으로 붙는다.
- 즉 자동 캡처 관측은 이미 프로젝트별로 나뉘어 있고, **수동 메모리만 prefix로 나눠주면 된다.**
  철자가 `ChatProject`로 일치하므로 이 한 단어로 양쪽이 함께 걸린다.

### 한계 — prefix는 필터가 아니다

recall은 hybrid BM25+vector 랭킹이다. prefix는 **순위를 올릴 뿐 다른 프로젝트를 배제하지 못한다.**
쿼리에 `ChatProject`가 없으면 `[BeanProfile]`, `[2d-prototype]` 메모리가 그대로 섞여 나온다.
저장 시 concepts, 조회 시 쿼리 양쪽에 `ChatProject`를 넣는 것이 이 규칙을 실제로 작동시키는 부분이다.

### 건드리지 말 것

- `AGENTMEMORY_PROJECT_NAME` — 전역 `~/.agentmemory/.env`에 넣으면 **모든** 프로젝트가
  ChatProject로 라벨된다. cwd basename이 이미 맞으므로 불필요하다.
- `AGENTMEMORY_AGENT_SCOPE=isolated` — 프로젝트가 아니라 **에이전트 ID** 격리다.
  `AGENT_ID`가 없으면 recall이 throw한다. 프로젝트 분리 용도로 쓸 수 없다.
- 기존 `[BeanProfile]` / `[2d-prototype]` 메모리 200개 — 이미 각자 프로젝트명을 달고 있어
  분리는 되고 있다. 소급 변경하지 않는다.
