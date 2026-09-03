# Domain-specific fixtures: Flask-style Route (`@app.route("/path")` decorator)
# and Response (`jsonify(...)` etc. — Flask's response-producing functions).

from flask import Flask, jsonify, render_template

app = Flask(__name__)


@app.route("/users")
def list_users():
    return jsonify([{"id": 1}])


@app.route("/users", methods=["POST"])
def create_user():
    return jsonify({"created": True})


@app.route("/health")
def health():
    return render_template("health.html")
