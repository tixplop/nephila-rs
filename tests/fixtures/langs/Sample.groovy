package com.acme.shop

import groovy.transform.CompileStatic

interface Named {
    String name()
}

enum Level { LOW, HIGH }

class Base {}

@CompileStatic
class Service extends Base implements Named {
    Repo repo;
    def run() {
        repo.save(new Item(name: 'a'))
        [1, 2].each { println it }
    }
}

def helper() { 42 }
