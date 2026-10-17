ALTER TABLE orders ADD COLUMN product_id INT REFERENCES shop.products (id);
