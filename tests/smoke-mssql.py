#!/usr/bin/env python3
"""Smoke test for MSSQL dialect on db-api."""

import json
import sys
import httpx

BASE_URL = "https://db-api.ljs.app"


def main():
    print("=== MSSQL Smoke Test ===\n")

    # 1. Create database
    print("1. Creating MSSQL database...")
    resp = httpx.post(f"{BASE_URL}/db/new", json={"dialect": "mssql"}, timeout=180)
    resp.raise_for_status()
    data = resp.json()
    db_id = data["db_id"]
    print(f"   Created: {db_id}")
    print(f"   Status: {data['status']}")

    try:
        # 2. Create table
        print("\n2. Creating table...")
        resp = httpx.post(
            f"{BASE_URL}/db/{db_id}/query",
            json={
                "query": """
                    CREATE TABLE users (
                        id INT IDENTITY(1,1) PRIMARY KEY,
                        name NVARCHAR(255) NOT NULL,
                        email NVARCHAR(255) NOT NULL,
                        created_at DATETIME2 DEFAULT GETDATE()
                    )
                """
            },
            timeout=30,
        )
        resp.raise_for_status()
        print("   Table 'users' created")

        # 3. Insert data
        print("\n3. Inserting data...")
        resp = httpx.post(
            f"{BASE_URL}/db/{db_id}/query",
            json={
                "query": """
                    INSERT INTO users (name, email) VALUES
                    ('Alice', 'alice@example.com'),
                    ('Bob', 'bob@example.com'),
                    ('Charlie', 'charlie@example.com')
                """
            },
            timeout=30,
        )
        resp.raise_for_status()
        print("   Inserted 3 rows")

        # 4. Select data
        print("\n4. Selecting data...")
        resp = httpx.post(
            f"{BASE_URL}/db/{db_id}/query",
            json={"query": "SELECT id, name, email FROM users ORDER BY id"},
            timeout=30,
        )
        resp.raise_for_status()
        data = resp.json()
        print(f"   Columns: {data['columns']}")
        print("   Rows:")
        for row in data["rows"]:
            print(f"     {row}")

        # 5. Verify row count from select
        print("\n5. Verifying row count...")
        row_count = len(data["rows"])
        print(f"   Total rows: {row_count}")

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
            resp = httpx.delete(f"{BASE_URL}/db/{db_id}", timeout=30)
            resp.raise_for_status()
            print("   Database destroyed")
        except Exception as e:
            print(f"   Warning: cleanup failed: {e}")


if __name__ == "__main__":
    sys.exit(main())
