package com.acme.core;

import com.acme.domain.Order;

public interface OrderRepository {
    void save(Order order);
}
