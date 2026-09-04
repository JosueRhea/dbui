-- Demo data: a small fictional storefront, enough to have something to click.
--
-- Load it into a database of its own, so it never touches anything you care
-- about and never collides with the live driver tests in `dbui_test`:
--
--   docker compose up -d
--   docker compose exec -T postgres psql -U postgres -d postgres \
--       -c 'CREATE DATABASE dbui_demo'
--   docker compose exec -T postgres psql -U postgres -d dbui_demo < docs/demo.sql
--
-- Then point a connection at 127.0.0.1:55432, user `postgres`, password `dbui`,
-- database `dbui_demo`. This is also what the README screenshots show.
--
-- Re-running it drops and rebuilds only the objects below.

DROP MATERIALIZED VIEW IF EXISTS top_products;
DROP VIEW IF EXISTS revenue_by_country;
DROP TABLE IF EXISTS payments, order_items, orders, products, customers;


CREATE TABLE customers (
    id           serial PRIMARY KEY,
    full_name    text NOT NULL,
    email        text NOT NULL UNIQUE,
    country      char(2) NOT NULL,
    plan         text NOT NULL DEFAULT 'free',
    lifetime_value numeric(10,2) NOT NULL DEFAULT 0,
    marketing_ok boolean NOT NULL DEFAULT false,
    metadata     jsonb,
    signed_up_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE products (
    id          serial PRIMARY KEY,
    sku         text NOT NULL UNIQUE,
    name        text NOT NULL,
    category    text NOT NULL,
    unit_price  numeric(10,2) NOT NULL,
    in_stock    integer NOT NULL DEFAULT 0,
    tags        text[],
    discontinued_at timestamptz
);

CREATE TABLE orders (
    id           serial PRIMARY KEY,
    customer_id  integer NOT NULL REFERENCES customers(id),
    status       text NOT NULL,
    total        numeric(10,2) NOT NULL,
    currency     char(3) NOT NULL DEFAULT 'USD',
    shipped_at   timestamptz,
    placed_at    timestamptz NOT NULL
);
CREATE INDEX orders_customer_id_idx ON orders(customer_id);

CREATE TABLE order_items (
    id         serial PRIMARY KEY,
    order_id   integer NOT NULL REFERENCES orders(id),
    product_id integer NOT NULL REFERENCES products(id),
    quantity   integer NOT NULL,
    unit_price numeric(10,2) NOT NULL
);

CREATE TABLE payments (
    id         serial PRIMARY KEY,
    order_id   integer NOT NULL REFERENCES orders(id),
    method     text NOT NULL,
    amount     numeric(10,2) NOT NULL,
    captured_at timestamptz,
    failure_reason text
);

CREATE VIEW revenue_by_country AS
SELECT c.country, count(*) AS orders, sum(o.total) AS revenue
FROM orders o JOIN customers c ON c.id = o.customer_id
WHERE o.status <> 'cancelled'
GROUP BY c.country ORDER BY revenue DESC;

CREATE MATERIALIZED VIEW top_products AS
SELECT p.id, p.name, sum(i.quantity) AS units
FROM order_items i JOIN products p ON p.id = i.product_id
GROUP BY p.id, p.name ORDER BY units DESC;

-- Customers -------------------------------------------------------------
INSERT INTO customers (full_name, email, country, plan, lifetime_value, marketing_ok, metadata, signed_up_at)
VALUES
 ('Maren Holloway','maren.holloway@example.com','US','pro',   4820.00, true,  '{"source":"referral","seats":12}', '2024-02-11 09:14:00+00'),
 ('Tobias Lindqvist','t.lindqvist@example.se','SE','team',   12940.50, true,  '{"source":"conference"}',        '2024-03-02 16:41:00+00'),
 ('Amara Okonkwo','amara@example.ng','NG','pro',              2310.75, false, '{"source":"organic"}',           '2024-03-19 11:02:00+00'),
 ('Rafael Mendes','rafael.mendes@example.br','BR','free',       0.00, false, NULL,                              '2024-04-07 21:35:00+00'),
 ('Yuki Tanabe','yuki.tanabe@example.jp','JP','team',        18775.25, true,  '{"source":"referral","seats":40}','2024-04-22 03:58:00+00'),
 ('Ingrid Bauer','ingrid.bauer@example.de','DE','pro',        6105.00, true,  '{"source":"ads"}',               '2024-05-14 08:20:00+00'),
 ('Noor Haddad','noor.haddad@example.ae','AE','free',           49.00, false, NULL,                             '2024-06-01 13:47:00+00'),
 ('Callum Fraser','callum.fraser@example.uk','GB','pro',      3960.40, true,  '{"source":"organic"}',           '2024-06-18 17:05:00+00');

INSERT INTO customers (full_name, email, country, plan, lifetime_value, marketing_ok, metadata, signed_up_at)
SELECT
  (ARRAY['Alina','Marcus','Priya','Diego','Hanna','Kenji','Sofia','Liam','Nadia','Omar','Elena','Theo'])[1 + (n % 12)]
    || ' ' ||
  (ARRAY['Vasquez','Norberg','Rahman','Costa','Lindgren','Sato','Moreau','Byrne','Petrova','Aziz','Kovac','Adeyemi'])[1 + ((n * 7) % 12)],
  'user' || n || '@example.com',
  (ARRAY['US','GB','DE','SE','JP','BR','NG','AE','FR','CA'])[1 + (n % 10)],
  (ARRAY['free','pro','team'])[1 + (n % 3)],
  round((n * 37.5)::numeric % 9000, 2),
  n % 3 = 0,
  CASE WHEN n % 4 = 0 THEN NULL ELSE jsonb_build_object('source', (ARRAY['organic','ads','referral'])[1 + (n % 3)]) END,
  timestamptz '2024-07-01 00:00:00+00' + (n || ' hours')::interval
FROM generate_series(1, 240) AS n;

-- Products --------------------------------------------------------------
INSERT INTO products (sku, name, category, unit_price, in_stock, tags, discontinued_at)
VALUES
 ('KB-0117','Aurora Mechanical Keyboard','peripherals', 149.00, 320, ARRAY['wireless','hot-swap'], NULL),
 ('MS-0042','Drift Ergonomic Mouse','peripherals',       79.00, 512, ARRAY['wireless'],            NULL),
 ('MN-2701','Meridian 27" 4K Display','displays',       549.00,  88, ARRAY['4k','usb-c'],          NULL),
 ('MN-3201','Meridian 32" Ultrawide','displays',        899.00,  24, ARRAY['ultrawide','usb-c'],   NULL),
 ('DK-0900','Anchor Thunderbolt Dock','accessories',    249.00, 140, ARRAY['thunderbolt'],         NULL),
 ('CB-0301','Braided USB-C Cable 2m','accessories',      19.00, 980, ARRAY['usb-c'],               NULL),
 ('HP-0550','Quiet Studio Headphones','audio',          229.00,  61, ARRAY['anc','bluetooth'],     NULL),
 ('WC-0110','Clearview 4K Webcam','video',              179.00,   0, ARRAY['4k'],                  '2025-01-15 00:00:00+00'),
 ('LT-0060','Halo Desk Lamp','lighting',                 89.00, 210, NULL,                         NULL),
 ('ST-0800','Riser Monitor Stand','furniture',           64.00, 175, ARRAY['aluminium'],           NULL);

-- Orders ----------------------------------------------------------------
INSERT INTO orders (customer_id, status, total, currency, shipped_at, placed_at)
SELECT
  1 + (n % 248),
  (ARRAY['paid','paid','paid','shipped','shipped','pending','refunded','cancelled'])[1 + (n % 8)],
  round((49 + (n * 13.75)::numeric % 1800), 2),
  (ARRAY['USD','USD','USD','EUR','GBP'])[1 + (n % 5)],
  CASE WHEN n % 3 = 0 THEN NULL ELSE timestamptz '2025-01-05 00:00:00+00' + (n || ' hours')::interval END,
  timestamptz '2025-01-04 00:00:00+00' + (n || ' hours')::interval
FROM generate_series(1, 1400) AS n;

INSERT INTO order_items (order_id, product_id, quantity, unit_price)
SELECT
  o.id,
  1 + ((o.id * k) % 10),
  1 + ((o.id + k) % 4),
  (ARRAY[149.00,79.00,549.00,899.00,249.00,19.00,229.00,179.00,89.00,64.00])[1 + ((o.id * k) % 10)]
FROM orders o CROSS JOIN generate_series(1, 2) AS k;

INSERT INTO payments (order_id, method, amount, captured_at, failure_reason)
SELECT
  o.id,
  (ARRAY['card','card','card','paypal','wire'])[1 + (o.id % 5)],
  o.total,
  CASE WHEN o.status IN ('paid','shipped') THEN o.placed_at + interval '4 minutes' ELSE NULL END,
  CASE WHEN o.status = 'cancelled' THEN 'insufficient_funds' ELSE NULL END
FROM orders o;

REFRESH MATERIALIZED VIEW top_products;
ANALYZE;
