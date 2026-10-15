package com.acme.gv

interface Shape {
    double area()
}

class Circle implements Shape {
    double area() { 3.14 }
}

trait Greeter {
    String greet() { 'hi' }
}
