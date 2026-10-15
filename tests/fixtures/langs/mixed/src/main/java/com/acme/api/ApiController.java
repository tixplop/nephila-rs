package com.acme.api;

public class ApiController {
    private com.acme.core.OrderService service;

    public void create() {
        service.place();
    }
}
