# q17: what the calculation sees of an ExecuteSQL / ExecuteSQLe error. File: FileMaker Server sample (en).
=EvaluationError ( ExecuteSQLe ( "SELECT x FROM Tasks" ; "" ; "" ) )
=EvaluationError ( ExecuteSQLe ( "SELEC" ; "" ; "" ) )
=EvaluationError ( ExecuteSQLe ( "SELECT 1/0 FROM Tasks" ; "" ; "" ) )
=EvaluationError ( ExecuteSQLe ( "SELECT Task FROM Tasks" ; "" ; "" ) )
=GetAsNumber ( ExecuteSQL ( "SELECT x FROM Tasks" ; "" ; "" ) )
=IsEmpty ( ExecuteSQL ( "SELECT x FROM Tasks" ; "" ; "" ) )
=ExecuteSQL ( "SELECT x FROM Tasks" ; "" ; "" ) & "|"
=Length ( ExecuteSQLe ( "SELECT x FROM Tasks" ; "" ; "" ) )
=IsValid ( ExecuteSQL ( "SELECT x FROM Tasks" ; "" ; "" ) )
=Let ( r = ExecuteSQL ( "SELECT x FROM Tasks" ; "" ; "" ) ; r = "?" )
