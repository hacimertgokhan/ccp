// CCP schema advertisement: sent once (on "SCHEMA"), not per call.
// Hand-written for now; a derive macro can generate this later from the
// Resolvable impls instead of hand-maintaining the string.
pub const SCHEMA: &str = "\
root user(id:Int):User\n\
root users:[User]\n\
root post(id:Int):Post\n\
type User{id:Int,name:Str,email:Str,posts:[Post]}\n\
type Post{id:Int,title:Str,body:Str,author_id:Int,author:User,comments:[Comment]}\n\
type Comment{id:Int,body:Str,author_name:Str}";
