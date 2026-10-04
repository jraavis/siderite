"""FastAPI Todo API counterpart to the Siderite ORM benchmark."""

import os
from contextlib import asynccontextmanager
from typing import Dict, List
from fastapi import FastAPI, HTTPException, status
from fastapi.concurrency import run_in_threadpool
from pydantic import BaseModel, Field
from sqlite_connections import SQLiteConnections
from database_settings import capture

# Global database connection / pool reference
db_backend: str = os.getenv("DB_BACKEND", "sqlite").lower()
pg_pool = None
mysql_pool = None
mongo_db = None
mongo_client = None
sqlite_path: str = os.getenv("SQLITE_PATH", "todo_bench.db")
pool_size: int = int(os.getenv("DATABASE_POOL_SIZE", "10"))
# Matches the Siderite app: WAL (set on the file by the runner) with
# synchronous = NORMAL on every connection.
sqlite_wal: bool = os.getenv("SQLITE_WAL") == "1"
sqlite_profile = os.getenv(
    "SQLITE_PROFILE", "wal-normal" if sqlite_wal else "default"
)
sqlite_mode = os.getenv("FASTAPI_SQLITE_MODE", "pooled")
sqlite_connections = SQLiteConnections(
    sqlite_path, sqlite_profile, sqlite_mode, pool_size
)


def sqlite_connect():
    return sqlite_connections.borrow()


async def sqlite_call(function, *args):
    """Run a blocking SQLite handler body for the configured strategy.

    ``pooled`` runs it on a worker thread so ``pool_size`` connections are
    used concurrently; the other strategies run it on the event loop.
    """
    if sqlite_mode == "pooled":
        return await run_in_threadpool(function, *args)
    return function(*args)


def sqlite_list():
    with sqlite_connect() as db:
        rows = db.execute(
            "SELECT id, title, done FROM todos ORDER BY id DESC LIMIT 20"
        ).fetchall()
        return [TodoOut(id=r[0], title=r[1], done=bool(r[2])) for r in rows]


def sqlite_get(todo_id):
    with sqlite_connect() as db:
        row = db.execute(
            "SELECT id, title, done FROM todos WHERE id = ?", (todo_id,)
        ).fetchone()
        if not row:
            raise HTTPException(status_code=404, detail="Todo not found")
        return TodoOut(id=row[0], title=row[1], done=bool(row[2]))


def sqlite_create(title):
    with sqlite_connect() as db:
        cur = db.execute("INSERT INTO todos (title, done) VALUES (?, 0)",
                         (title,))
        return TodoOut(id=cur.lastrowid, title=title, done=False)


class TodoCreate(BaseModel):
    title: str = Field(min_length=1, max_length=280)


class TodoOut(BaseModel):
    id: int
    title: str
    done: bool


@asynccontextmanager
async def lifespan(app: FastAPI):
    global pg_pool, mysql_pool, mongo_db, mongo_client
    if os.getenv("BENCHMARK_RUNTIME"):
        import json
        print("BENCHMARK_RUNTIME " + json.dumps({
            "scheduler_workers": 1, "pid": os.getpid(),
        }), flush=True)

    if db_backend == "postgres":
        import asyncpg

        pg_url = os.getenv(
            "DATABASE_URL",
            "postgres://siderite:siderite@127.0.0.1:55432/siderite",
        )
        pg_pool = await asyncpg.create_pool(
            pg_url, min_size=pool_size, max_size=pool_size
        )
        async with pg_pool.acquire() as conn:
            await conn.execute(
                """
                CREATE TABLE IF NOT EXISTS todos (
                    id BIGSERIAL PRIMARY KEY,
                    title VARCHAR(280) NOT NULL,
                    done BOOLEAN NOT NULL DEFAULT FALSE
                );
            """
            )
    elif db_backend == "mysql":
        import aiomysql
        from pymysql.constants import CLIENT

        # Support mysql://user:pass@host:port/db URL or components
        mysql_url = os.getenv("MYSQL_URL") or os.getenv("DATABASE_URL", "")
        if mysql_url.startswith("mysql://"):
            import urllib.parse

            url_parts = urllib.parse.urlparse(mysql_url)
            host = url_parts.hostname or "127.0.0.1"
            port = url_parts.port or 53306
            user = urllib.parse.unquote(url_parts.username or "root")
            password = urllib.parse.unquote(url_parts.password or "")
            db = url_parts.path.lstrip("/") or "siderite"
        else:
            host = "127.0.0.1"
            port = int(os.getenv("MYSQL_PORT", "53306"))
            user = os.getenv("MYSQL_USER", "root")
            password = os.getenv("MYSQL_PASSWORD", "siderite")
            db = os.getenv("MYSQL_DATABASE", "siderite")

        mysql_pool = await aiomysql.create_pool(
            host=host,
            port=port,
            user=user,
            password=password,
            db=db,
            minsize=pool_size,
            maxsize=pool_size,
            autocommit=True,
            client_flag=CLIENT.FOUND_ROWS,
            init_command=(
                "SET SESSION time_zone = '+00:00', "
                "sql_mode = REPLACE(@@sql_mode, 'NO_BACKSLASH_ESCAPES', ''), "
                "group_concat_max_len = 4294967295"
            ),
        )
        async with mysql_pool.acquire() as conn:
            async with conn.cursor() as cur:
                await cur.execute(
                    """
                    CREATE TABLE IF NOT EXISTS todos (
                        id BIGINT PRIMARY KEY AUTO_INCREMENT,
                        title VARCHAR(280) NOT NULL,
                        done BOOLEAN NOT NULL DEFAULT FALSE
                    );
                """
                )
    elif db_backend == "mongodb":
        from motor.motor_asyncio import AsyncIOMotorClient

        mongo_url = os.getenv(
            "MONGODB_URL",
            "mongodb://127.0.0.1:57017/siderite?directConnection=true",
        )
        mongo_client = AsyncIOMotorClient(mongo_url, maxPoolSize=pool_size)
        mongo_db = mongo_client.get_default_database("siderite")
    elif db_backend == "sqlite":
        if sqlite_mode == "pooled":
            # Starlette's default worker limit must not cap the pool.
            import anyio.to_thread
            limiter = anyio.to_thread.current_default_thread_limiter()
            limiter.total_tokens = max(limiter.total_tokens, pool_size)
        sqlite_connections.open()
        with sqlite_connect() as db:
            db.execute(
                """
                CREATE TABLE IF NOT EXISTS todos (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    title TEXT NOT NULL,
                    done INTEGER NOT NULL DEFAULT 0
                );
            """
            )
            db.commit()

    try:
        if os.getenv("BENCHMARK_RUNTIME"):
            import json
            settings = await capture(
                db_backend, sqlite_connections, pg_pool, mysql_pool, mongo_db
            )
            if settings is not None:
                print("BENCHMARK_DATABASE " + json.dumps({
                    "pid": os.getpid(), "settings": settings,
                }), flush=True)
        yield
    finally:
        if pg_pool:
            await pg_pool.close()
        if mysql_pool:
            mysql_pool.close()
            await mysql_pool.wait_closed()
        if mongo_client:
            mongo_client.close()
        sqlite_connections.close()



app = FastAPI(
    lifespan=lifespan,
    openapi_url=None,
    docs_url=None,
    redoc_url=None,
)


@app.get("/health")
def health() -> Dict[str, str]:
    return {"status": "ok", "backend": db_backend}


@app.get("/todos", response_model=List[TodoOut])
async def list_todos():
    if db_backend == "postgres":
        async with pg_pool.acquire() as conn:
            rows = await conn.fetch(
                "SELECT id, title, done FROM todos "
                    "ORDER BY id DESC LIMIT 20"
            )
            return [
                TodoOut(id=r["id"], title=r["title"], done=r["done"])
                for r in rows
            ]
    elif db_backend == "mysql":
        async with mysql_pool.acquire() as conn:
            async with conn.cursor() as cur:
                await cur.execute(
                    "SELECT id, title, done FROM todos "
                    "ORDER BY id DESC LIMIT 20"
                )
                rows = await cur.fetchall()
                return [
                    TodoOut(id=r[0], title=r[1], done=bool(r[2])) for r in rows
                ]
    elif db_backend == "mongodb":
        cursor = mongo_db["todos"].find().sort("_id", -1).limit(20)
        results = []
        async for doc in cursor:
            results.append(
                TodoOut(id=doc["_id"], title=doc["title"], done=doc["done"])
            )
        return results
    elif db_backend == "sqlite":
        return await sqlite_call(sqlite_list)
    raise HTTPException(status_code=500, detail="Unsupported backend")


@app.get("/todos/{todo_id}", response_model=TodoOut)
async def get_todo(todo_id: int):
    if db_backend == "postgres":
        async with pg_pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT id, title, done FROM todos WHERE id = $1", todo_id
            )
            if not row:
                raise HTTPException(status_code=404, detail="Todo not found")
            return TodoOut(id=row["id"], title=row["title"], done=row["done"])
    elif db_backend == "mysql":
        async with mysql_pool.acquire() as conn:
            async with conn.cursor() as cur:
                await cur.execute(
                    "SELECT id, title, done FROM todos WHERE id = %s",
                    (todo_id,),
                )
                row = await cur.fetchone()
                if not row:
                    raise HTTPException(
                        status_code=404, detail="Todo not found"
                    )
                return TodoOut(id=row[0], title=row[1], done=bool(row[2]))
    elif db_backend == "mongodb":
        doc = await mongo_db["todos"].find_one({"_id": todo_id})
        if not doc:
            raise HTTPException(status_code=404, detail="Todo not found")
        return TodoOut(id=doc["_id"], title=doc["title"], done=doc["done"])
    elif db_backend == "sqlite":
        return await sqlite_call(sqlite_get, todo_id)
    raise HTTPException(status_code=500, detail="Unsupported backend")


@app.post(
    "/todos", response_model=TodoOut, status_code=status.HTTP_201_CREATED
)
async def create_todo(todo: TodoCreate):
    if db_backend == "postgres":
        async with pg_pool.acquire() as conn:
            row = await conn.fetchrow(
                "INSERT INTO todos (title, done) VALUES ($1, FALSE) "
                "RETURNING id, title, done",
                todo.title,
            )
            return TodoOut(id=row["id"], title=row["title"], done=row["done"])
    elif db_backend == "mysql":
        async with mysql_pool.acquire() as conn:
            async with conn.cursor() as cur:
                await cur.execute(
                    "INSERT INTO todos (title, done) VALUES (%s, FALSE)",
                    (todo.title,),
                )
                inserted_id = cur.lastrowid
                return TodoOut(id=inserted_id, title=todo.title, done=False)
    elif db_backend == "mongodb":
        # Atomically increment integer counter
        counter = await mongo_db["siderite_counters"].find_one_and_update(
            {"_id": "todos"},
            {"$inc": {"seq": 1}},
            upsert=True,
            return_document=True,
        )
        new_id = counter["seq"]
        await mongo_db["todos"].insert_one(
            {"_id": new_id, "title": todo.title, "done": False}
        )
        return TodoOut(id=new_id, title=todo.title, done=False)
    elif db_backend == "sqlite":
        return await sqlite_call(sqlite_create, todo.title)
    raise HTTPException(status_code=500, detail="Unsupported backend")


if __name__ == "__main__":
    import argparse
    import uvicorn

    parser = argparse.ArgumentParser(
        description="FastAPI Todo Benchmark Server"
    )
    parser.add_argument("--host", default="127.0.0.1", help="Host to bind to")
    parser.add_argument(
        "--port", type=int, default=8082, help="Port to bind to"
    )
    parser.add_argument(
        "--backend",
        default="sqlite",
        choices=["sqlite", "postgres", "mysql", "mongodb"],
        help="Database backend",
    )
    args = parser.parse_args()

    os.environ["DB_BACKEND"] = args.backend

    uvicorn.run(
        "todo_app:app",
        host=args.host,
        port=args.port,
        workers=1,
        log_level="warning",
        access_log=False,
    )
