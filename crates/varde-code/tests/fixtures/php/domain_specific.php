<?php
// Domain-specific fixtures: Laravel-style Route (a `Route::<verb>` static call
// with a string path) and Response (`view`/`json`/`response()->json` — Laravel's
// response-producing helpers, body_shape set to the helper name).

Route::get('/users', 'UserController@index');
Route::post('/users', 'UserController@store');

function index()
{
    return view('users.index');
}

function store()
{
    return response()->json(['ok' => true]);
}
