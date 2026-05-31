# std/env

Environment variable access and modification.

```lin
import { getEnv, setEnv, unsetEnv, environ } from "std/env"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `environ` | `() -> { ...String }` | All environment variables as an object |
| `getEnv` | `(String) -> String \| Null` | Value of a variable, or Null |
| `setEnv` | `(String, String) -> Null` | Set a variable |
| `unsetEnv` | `(String) -> Null` | Unset a variable |

---

### `getEnv`

```lin
val home = getEnv("HOME")    // e.g. "/home/alice" or null
val port = getEnv("PORT")
val p = match port
  is Null => 3000
  else    => parseInt32(port)
```

---

### `environ`

```lin
val env = environ()
print(env["HOME"])
print(env["PATH"])
```

---

### `setEnv`

```lin
setEnv("APP_ENV", "production")
setEnv("LOG_LEVEL", "debug")
```

Affects the current process and any child processes spawned after this call.

---

### `unsetEnv`

```lin
unsetEnv("DEBUG")
```

No-op if the variable is not set.
