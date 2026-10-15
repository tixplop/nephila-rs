package com.acme.shop

import scala.collection.mutable.{Map => MMap, ListBuffer}

trait Named { def name: String }
abstract class Base extends Named with Serializable
case class Item(name: String, qty: Int) extends Base
object Registry {
  val items = ListBuffer[Item]()
  def add(i: Item): Unit = items += i
}
class Service(repo: Repo) {
  def run(): Unit = repo.save(Item("a", 1))
}
