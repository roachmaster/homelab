# Forge llama.cpp adapter architecture

These views describe the implemented adapter and its Forge configuration. The [usage guide](README.md) records the hardware test and current recovery status.

## C4 level 1 — System context

```mermaid
flowchart LR
    Operator["Operator"]
    Client["REST client<br/>Owns conversation history"]
    Forge["Forge GPU inference<br/>Selects a local model and serves chat"]
    Operator -->|"Chooses a model and sends messages"| Client
    Client -->|"Bearer-authenticated JSON over SSH tunnel"| Forge
    Forge -->|"Model status, replies and usage"| Client
```

A caller on Forge can use loopback directly. The SSH tunnel is the remote path shown here.

## C4 level 2 — Containers on Forge GPU

```mermaid
flowchart TB
    Client["Client<br/>Message history"]
    subgraph Forge["Forge GPU host"]
        REST["forge-llama<br/>Rust REST service · 127.0.0.1:18080"]
        Llama["llama-server<br/>Owned child · 127.0.0.1:18081"]
        Files["Local GGUF files<br/>GPT-OSS 20B and Qwen 2.5 Coder 7B"]
        GPU["Intel Arc Pro B50"]
        REST -->|"Launches and reaps one child"| Llama
        REST -->|"HTTP · health and chat"| Llama
        Llama -->|"Loads the selected GGUF"| Files
        Llama -->|"SYCL0 · inference"| GPU
    end
    Client -->|"SSH tunnel to port 18080"| REST
```

The user systemd unit starts the adapter. The adapter starts a model only after an admitted selection. The GPU lock coordinates processes that use the same lock file.

## C4 level 3 — Components inside the adapter

```mermaid
flowchart LR
    Caller["HTTP caller"]
    Backend["Owned llama-server"]
    subgraph Adapter["forge-llama process"]
        HTTP["Http<br/>Bearer check, bounded JSON and route mapping"]
        Slot["Command slot<br/>One admitted job at a time"]
        Worker["Dedicated worker<br/>Owns synchronous dispatch"]
        Bridge["Forge Bridge<br/>Header lookup, admission and variant check"]
        Driver["Driver<br/>Model policy, lifecycle and upstream HTTP"]
        Factory["LlamaFactory<br/>Staged in the bootstrap registry"]
        HTTP -->|"Typed Envelope and one-shot reply"| Slot
        Slot --> Worker
        Worker --> Bridge
        Bridge -->|"Registered binding"| Driver
        Factory -->|"Creates after registry freeze"| Driver
    end
    Caller --> HTTP
    Driver -->|"Owns child and sends local requests"| Backend
```

The transport selects the route by method and path. The body supplies typed data for that route. The worker owns the driver in a singleton. See [HTTP routing and worker assembly](src/main.rs) and [the typed route bindings](src/lib.rs).

## UML — Ownership and chat data

```mermaid
classDiagram
    class LlamaFactory {
        +create()
    }
    class Driver {
        -start(model)
        -stop_child()
        -status()
        -chat(chat)
    }
    class Config {
        +models
        +device
        +backend_port
        +gpu_lock
    }
    class Running {
        -child
        -model
        -gpu_lease
    }
    class Chat {
        +model
        +messages
        +max_tokens
        +temperature
    }
    class Message {
        +role
        +content
    }
    class Completion {
        +choices
        +usage
        +timings
    }
    LlamaFactory ..> Driver : creates
    Driver *-- "1" Config : owns
    Driver *-- "0..1" Running : owns while ready
    Driver ..> Chat : borrows for request
    Driver ..> Completion : returns
    Chat *-- "0..*" Message : owns input list
```

The input type can represent an empty list. Chat validation requires 1–64 messages with at least one user message. A child still loading is held by the start operation and moves into the driver's `Running` field after readiness. See [driver ownership](src/driver.rs).

## UML sequence — Start or switch a model

```mermaid
sequenceDiagram
    participant Client
    participant REST as REST adapter
    participant Bridge as Worker and Forge bridge
    participant Driver
    participant Backend as llama-server child
    Client->>REST: PUT /v1/runtime with model ID
    REST->>REST: Verify token and decode selection
    alt Missing or invalid token
        REST-->>Client: 401 unauthorized
    else Another command owns the slot
        REST-->>Client: 409 runtime_busy
    else Command admitted
        REST->>Bridge: llama.start envelope
        Bridge->>Driver: Validate model, GGUF and GPU presence
        alt Validation fails
            Driver-->>Bridge: Unknown model or unavailable
        else Requested model already ready
            Driver-->>Bridge: Current model
        else Start or replace
            Driver->>Driver: Acquire or retain GPU lock
            opt Existing child
                Driver->>Backend: SIGTERM and wait
                Backend-->>Driver: Exit and reap
            end
            Driver->>Backend: Spawn selected model
            loop Until ready, exit or deadline
                Driver->>Backend: GET /health
                Backend-->>Driver: Loading or ready
            end
            alt Ready
                Driver-->>Bridge: Ready status
            else Startup fails after old child exits
                Driver->>Driver: Clean up replacement and release lock
                Driver-->>Bridge: Startup error
            end
        end
        Bridge-->>REST: Typed result or error
        REST-->>Client: HTTP status and JSON
    end
```

An unknown model maps to HTTP 404, and an unavailable GPU maps to HTTP 503. A lock conflict or shutdown failure can also end the operation before spawning a replacement.

## UML sequence — Continue a chat

```mermaid
sequenceDiagram
    participant Client as Client and history
    participant REST as REST adapter
    participant Bridge as Worker and Forge bridge
    participant Driver
    participant Llama as llama-server
    Client->>REST: POST chat with user question
    REST->>Bridge: llama.chat envelope
    Bridge->>Driver: Validated route and variant
    Driver->>Llama: Chat completion with messages
    Llama-->>Driver: Assistant reply 42
    Driver-->>REST: Completion
    REST-->>Client: Reply 42
    Client->>Client: Append assistant reply and next user turn
    Client->>REST: POST chat with history plus Now double that
    REST->>Bridge: New llama.chat envelope
    Bridge->>Driver: Validated route and variant
    Driver->>Llama: Chat completion with full supplied history
    Llama-->>Driver: Assistant reply 84
    Driver-->>REST: Completion
    REST-->>Client: Reply 84
    Client->>Client: Append assistant reply
```

The second turn is illustrative. The adapter sends each request upstream with `stream: false` and `cache_prompt: false`. It checks that a supplied model ID matches the ready model. A new request gets its own Forge envelope. The client saves the messages when it wants to resume a conversation later.

## UML state — Model lifecycle

```mermaid
stateDiagram-v2
    [*] --> Stopped
    Stopped --> Starting: Valid start and acquired GPU lock
    Starting --> Ready: Health ready and child alive
    Starting --> Stopped: Child exits or startup times out and cleanup completes
    Ready --> Ready: Chat succeeds or same model selected
    Ready --> Stopped: Child exit observed or stop and reap succeeds
    Ready --> Stopping: Switch to a different valid model
    Stopping --> Starting: Old child reaped and lock retained
    Stopping --> Stopped: Old child reaped but replacement cannot start
```

`Starting` and `Stopping` are internal phases. The REST status response exposes only `ready` or `stopped`, and a concurrent command can receive `409 runtime_busy`. When the upstream HTTP exchange times out or fails, the driver attempts to stop the child before accepting another command.
