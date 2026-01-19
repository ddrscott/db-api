#!/usr/bin/env python3
"""Smoke test for MSSQL dialect on db-api."""

import sys
import time
import httpx

# Use environment variable or default to local
import os
BASE_URL = os.environ.get("DB_API_URL", "https://db-api.ljs.app")


def timed(func):
    """Decorator to time function execution."""
    def wrapper(*args, **kwargs):
        start = time.perf_counter()
        result = func(*args, **kwargs)
        elapsed = time.perf_counter() - start
        return result, elapsed
    return wrapper


@timed
def create_database(dialect: str) -> dict:
    resp = httpx.post(f"{BASE_URL}/db/new", json={"dialect": dialect}, timeout=180)
    resp.raise_for_status()
    return resp.json()


@timed
def run_query(db_id: str, query: str) -> dict:
    resp = httpx.post(
        f"{BASE_URL}/db/{db_id}/query",
        json={"query": query},
        timeout=30,
    )
    resp.raise_for_status()
    return resp.json()


@timed
def destroy_database(db_id: str) -> dict:
    resp = httpx.delete(f"{BASE_URL}/db/{db_id}", timeout=30)
    resp.raise_for_status()
    return resp.json()


def main():
    print("=== MSSQL Smoke Test ===\n")
    timings = []

    # 1. Create database
    print("1. Creating MSSQL database...")
    data, elapsed = create_database("mssql")
    db_id = data["db_id"]
    print(f"   Created: {db_id}")
    print(f"   Status: {data['status']}")
    print(f"   Time: {elapsed:.2f}s")
    timings.append(("Create database", elapsed))

    try:
        # 2. Create table
        print("\n2. Creating table...")
        _, elapsed = run_query(db_id, """
            CREATE TABLE users (
                id INT IDENTITY(1,1) PRIMARY KEY,
                name NVARCHAR(255) NOT NULL,
                email NVARCHAR(255) NOT NULL,
                created_at DATETIME2 DEFAULT GETDATE()
            )
        """)
        print("   Table 'users' created")
        print(f"   Time: {elapsed:.3f}s")
        timings.append(("Create table", elapsed))

        # 3. Insert data
        print("\n3. Inserting data...")
        _, elapsed = run_query(db_id, """
            INSERT INTO users (name, email) VALUES
            ('Alice', 'alice@example.com'),
            ('Bob', 'bob@example.com'),
            ('Charlie', 'charlie@example.com')
        """)
        print("   Inserted 3 rows")
        print(f"   Time: {elapsed:.3f}s")
        timings.append(("Insert data", elapsed))

        # 4. Select data
        print("\n4. Selecting data...")
        data, elapsed = run_query(db_id, "SELECT id, name, email FROM users ORDER BY id")
        print(f"   Columns: {data['columns']}")
        print("   Rows:")
        for row in data["rows"]:
            print(f"     {row}")
        print(f"   Time: {elapsed:.3f}s")
        timings.append(("Select data", elapsed))

        # 5. Verify row count from select
        print("\n5. Verifying row count...")
        row_count = len(data["rows"])
        print(f"   Total rows: {row_count}")

        # Print timing summary
        print("\n--- Timing Summary ---")
        total = 0
        for name, t in timings:
            print(f"   {name}: {t:.3f}s")
            total += t
        print(f"   Total: {total:.2f}s")

        if row_count == 3:
            print("\n=== PASSED ===")
            return 0
        else:
            print(f"\n=== FAILED: Expected 3 rows, got {row_count} ===")
            return 1

    finally:
        # Cleanup: destroy database
        print(f"\nCleaning up: destroying database {db_id}...")
        try:
            _, elapsed = destroy_database(db_id)
            print(f"   Database destroyed ({elapsed:.3f}s)")
        except Exception as e:
            print(f"   Warning: cleanup failed: {e}")


if __name__ == "__main__":
    sys.exit(main())
