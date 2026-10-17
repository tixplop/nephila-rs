CREATE TABLE customers (id INT PRIMARY KEY, name TEXT);
CREATE TABLE orders (
  id INT PRIMARY KEY,
  customer_id INT REFERENCES customers (id)
);
