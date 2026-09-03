// Structural fixtures: Interface (trait), Class (class/case class/object),
// Function (def), Variable (val/var), Parameter, Extends (`extends`),
// Implements (`with`), Call, MemberAccess, Literal, and Import.

import com.example.util.Helper
import scala.collection.mutable.{ListBuffer, Map}

trait Repository {
  def save(): Unit
}

case class User(id: Int, name: String)

class UserService(repo: Repository) extends BaseService with Logging {
  val prefix = "user:"
  var count = 0

  def register(name: String): User = {
    count = count + 1
    val user = User(count, name)
    repo.save()
    user
  }
}
